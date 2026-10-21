//! The one socket under quinn.
//!
//! A profile has one UDP socket, and every link of the profile is a QUIC
//! connection on it. quinn never sees the network: it sends to a stand-in
//! address, one per peer, and [`LinkSocket`] puts each packet on the path
//! chosen for that peer: straight to one of its addresses, through a
//! forwarding link (the relay's or a paired daemon's) behind a 17-byte
//! header, or over a byte stream. Arrivals are mapped back to the peer's
//! stand-in whichever path brought them, so a path change is invisible to
//! quinn and to every stream open on the link.
//!
//! Our own datagrams (the forwarding header, probes and their answers) are
//! told apart from QUIC by the fixed bit, which QUIC sets on every packet
//! unless the peer agreed to grease it. No endpoint of ours offers that, so
//! a clear fixed bit always means one of ours.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll, Waker};

pub use path_core::{LinkId, Path, PathId};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::sync::mpsc;

use super::framed::FramedKind;
use super::link_log::PathKind;
use crate::HostId;

/// What the socket layer sits on: the OS socket in production, the
/// testnet's router imitation in stories. Addresses are real; a router
/// imitation rewrites them as a NAT would.
pub trait Datagrams: Send + Sync + 'static {
    /// Sends one datagram without waiting; `WouldBlock` when the substrate
    /// is full, after which [`Datagrams::poll_send_ready`] wakes the sender.
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()>;
    fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let _ = cx;
        Poll::Ready(Ok(()))
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
}

/// QUIC sets this bit in the first byte of every packet, long header or
/// short; our own datagrams clear it.
const FIXED_BIT: u8 = 0x40;
const KIND_FORWARDED: u8 = 0x00;
const KIND_PROBE: u8 = 0x01;
const KIND_PROBE_ANSWER: u8 = 0x02;
const KIND_DELIVERED: u8 = 0x03;

/// The bytes forwarding adds in front of a QUIC packet: the kind and a
/// host id, the same on the way to a forwarder and from it.
pub const FORWARD_HEADER: usize = 1 + 16;
const PROBE_LEN: usize = 1 + 16 + 8;

/// The largest QUIC datagram on every path. QUIC's smallest, so it fits on
/// every IPv6 path even behind the forwarding header (1,217 of the 1,232
/// bytes IPv6's minimum MTU leaves for UDP), and a path switch never needs
/// a new path MTU search.
pub const MAX_DATAGRAM: usize = 1200;

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

/// How many packets that arrived on byte-stream paths may wait for quinn,
/// across every such path of the socket; past it the oldest is dropped, as
/// a full UDP receive buffer drops, and QUIC sends it again.
const FRAMED_RECV_QUEUE: usize = 1024;

/// How many receive errors in a row one poll skips before it yields, so a
/// substrate failing every receive cannot hold the runtime's thread.
const RECV_ERRORS_PER_POLL: usize = 64;

/// What the socket dropped rather than hand quinn an error, which would
/// end a connection (a send) or the endpoint (a receive).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drops {
    /// Sends that failed on the carrying path, or had nowhere to go.
    pub sends: u64,
    /// Receive errors the substrate reported.
    pub receive_errors: u64,
    /// Byte-stream arrivals dropped from a full receive queue.
    pub framed_arrivals: u64,
}

#[derive(Default)]
struct DropCounts {
    sends: AtomicU64,
    receive_errors: AtomicU64,
    framed_arrivals: AtomicU64,
}

/// A byte-stream path's sending half. Sending queues the packet and never
/// waits; a full queue drops it, as a full UDP buffer would. The stream's
/// reader hands each arriving packet to [`LinkSocket::deliver_framed`].
pub trait FramedIo: Send + Sync + 'static {
    fn send(&self, packet: &[u8]) -> io::Result<()>;
    fn kind(&self) -> FramedKind;
}

/// Where a forwarder's socket hands the packets other hosts send it to
/// forward: each with the stand-in of the sender, the host its header
/// names and the packet behind the header.
pub trait ForwardSink: Send + Sync + 'static {
    fn arrived(&self, sender: SocketAddr, dst: HostId, payload: &[u8]);
}

/// A probe's answer arrived; the nonce names the path it was sent on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeAnswered {
    pub from: HostId,
    pub nonce: u64,
}

/// Stand-ins live in a unique-local IPv6 prefix (`fd61:6d75:7800::/48`)
/// that no real path uses, so a stand-in is never mistaken for an address.
const STAND_IN_PREFIX: [u16; 3] = [0xfd61, 0x6d75, 0x7800];
const STAND_IN_PORT: u16 = 1;

fn stand_in_addr(index: u64) -> SocketAddr {
    let [a, b, c] = STAND_IN_PREFIX;
    let ip = Ipv6Addr::new(
        a,
        b,
        c,
        0,
        (index >> 48) as u16,
        (index >> 32) as u16,
        (index >> 16) as u16,
        index as u16,
    );
    SocketAddr::new(IpAddr::V6(ip), STAND_IN_PORT)
}

/// Who a stand-in stands for. A sender that has not said who it is (the
/// first packets of a link someone dialled to us) is known only by the path
/// it came on, until [`LinkSocket::name`] gives it its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Peer {
    Host(HostId),
    Unnamed(Path),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Route {
    /// Nowhere to send yet: quinn's packets are dropped, as a lost packet
    /// would be, and quinn sends them again.
    #[default]
    Unset,
    Carrying(Path),
    /// Every candidate at once, until one carries the link. The peer drops
    /// the copies as duplicate packets.
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

/// A path that is a network address of the peer's own, rather than a
/// forwarder or a byte stream.
fn addressed(path: Path) -> bool {
    matches!(path, Path::Direct(_) | Path::Punched(_))
}

struct PeerState {
    /// Where quinn hears the peer now. A host's earlier stand-ins (an
    /// unnamed sender's it was later known by) stay mapped to it, so a
    /// connection quinn still holds on one keeps reaching it.
    stand_in: SocketAddr,
    route: Route,
}

/// Where an arrival came from, before the stand-in mapping.
#[derive(Clone, Copy, Debug)]
enum Arrival {
    Udp(SocketAddr),
    Framed(PathId),
}

#[derive(Default)]
struct State {
    next_stand_in: u64,
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
    forwarding: HashMap<LinkId, (SocketAddr, PathKind)>,
    framed: HashMap<PathId, Arc<dyn FramedIo>>,
    next_path: u64,
    inbound: VecDeque<(PathId, Vec<u8>)>,
    recv_waker: Option<Waker>,
    probe_answers: Option<mpsc::UnboundedSender<ProbeAnswered>>,
    forward_sink: Option<Arc<dyn ForwardSink>>,
}

impl State {
    fn stand_in(&mut self, peer: Peer) -> SocketAddr {
        if let Some(state) = self.peers.get(&peer) {
            return state.stand_in;
        }
        let stand_in = stand_in_addr(self.next_stand_in);
        self.next_stand_in += 1;
        let route = match peer {
            Peer::Host(_) => Route::Unset,
            Peer::Unnamed(path) => Route::Carrying(path),
        };
        self.peers.insert(peer, PeerState { stand_in, route });
        self.by_stand_in.insert(stand_in, peer);
        stand_in
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

    /// Forgets `host`: its stand-ins, its addresses and the forwarding
    /// links it was the forwarder of.
    fn forget(&mut self, host: HostId) {
        let peer = Peer::Host(host);
        let by_stand_in = &self.by_stand_in;
        self.forwarding
            .retain(|_, (via, _)| by_stand_in.get(via) != Some(&peer));
        self.by_stand_in.retain(|_, held| *held != peer);
        self.peers.remove(&peer);
        self.unlearn_addresses(host);
    }

    /// Carries `host` on `route`. Addresses its earlier routes took still
    /// lead to it, so packets still in flight on a path it switched from
    /// arrive as its own.
    fn set_route(&mut self, host: HostId, route: Route) {
        self.stand_in(Peer::Host(host));
        for path in route.paths() {
            self.learn(host, *path);
        }
        self.peers
            .get_mut(&Peer::Host(host))
            .expect("stand-in just made")
            .route = route;
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

/// quinn's socket: one stand-in address per peer, one route per peer.
/// quinn sends to the stand-in; the layer puts the packet on the carrying
/// path, or on every candidate while the route sends on all, and maps
/// arrivals back to the stand-in whichever path brought them.
pub struct LinkSocket {
    /// Swapped whole when the network under the profile changes.
    substrate: RwLock<Arc<dyn Datagrams>>,
    own: HostId,
    state: Mutex<State>,
    drops: DropCounts,
}

impl fmt::Debug for LinkSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkSocket")
            .field("own", &self.own)
            .field("local_addr", &self.substrate().local_addr().ok())
            .finish_non_exhaustive()
    }
}

impl LinkSocket {
    pub fn bind(substrate: Arc<dyn Datagrams>, own: HostId) -> io::Result<Arc<Self>> {
        substrate.local_addr()?;
        Ok(Arc::new(Self {
            substrate: RwLock::new(substrate),
            own,
            state: Mutex::new(State::default()),
            drops: DropCounts::default(),
        }))
    }

    /// Binds the profile's UDP socket: `[::]` accepting IPv4 too when `bind`
    /// is unspecified, or `0.0.0.0` when the OS has no IPv6. With port 0 it
    /// tries `remembered_port` first, so peers that remember the profile's
    /// address find it again after a restart.
    pub fn bind_dual_stack(
        bind: SocketAddr,
        remembered_port: Option<u16>,
    ) -> io::Result<Arc<dyn Datagrams>> {
        Self::on_udp(bind_udp(bind, remembered_port)?)
    }

    /// The substrate over a UDP socket the caller already bound.
    pub fn on_udp(socket: std::net::UdpSocket) -> io::Result<Arc<dyn Datagrams>> {
        socket.set_nonblocking(true)?;
        Ok(Arc::new(Udp::new(tokio::net::UdpSocket::from_std(
            socket,
        )?)?))
    }

    pub fn own(&self) -> HostId {
        self.own
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.substrate().local_addr()
    }

    /// Moves the socket onto a new substrate, for a host whose network
    /// changed under it. Stand-ins and routes stay; quinn must be told with
    /// `Endpoint::rebind_abstract` so it polls the new substrate.
    pub fn rebind(&self, substrate: Arc<dyn Datagrams>) {
        *self
            .substrate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = substrate;
        if let Some(waker) = self.lock().recv_waker.take() {
            waker.wake();
        }
    }

    fn substrate(&self) -> Arc<dyn Datagrams> {
        self.substrate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The address quinn dials and hears `peer` at. It changes only when
    /// the peer is named afresh at a stand-in it was first heard at
    /// unnamed; the earlier one keeps reaching it.
    pub fn stand_in(&self, peer: HostId) -> SocketAddr {
        self.lock().stand_in(Peer::Host(peer))
    }

    /// The host behind a stand-in, if it has said who it is.
    pub fn peer_of(&self, stand_in: SocketAddr) -> Option<HostId> {
        match self.lock().by_stand_in.get(&stand_in)? {
            Peer::Host(host) => Some(*host),
            Peer::Unnamed(_) => None,
        }
    }

    /// The address quinn dials to reach whoever answers at `addr`, before
    /// the handshake says who that is: the stand-in arrivals from `addr`
    /// map to, so the dial and its answers meet on one address.
    pub fn stand_in_at(&self, addr: SocketAddr) -> SocketAddr {
        let addr = plain(addr);
        let mut state = self.lock();
        let peer = state.sender(Arrival::Udp(addr));
        state.stand_in(peer)
    }

    /// The address quinn dials to reach whoever answers on the byte-stream
    /// path `path`: the stand-in arrivals on that path map to.
    pub fn stand_in_on(&self, path: PathId) -> SocketAddr {
        let mut state = self.lock();
        let peer = state.sender(Arrival::Framed(path));
        state.stand_in(peer)
    }

    /// The network address behind a stand-in, where it is one: an unnamed
    /// sender's address, or the address carrying a host's link. None for a
    /// byte stream or a forwarder.
    pub fn address_of(&self, stand_in: SocketAddr) -> Option<SocketAddr> {
        let state = self.lock();
        let peer = state.by_stand_in.get(&stand_in)?;
        let path = match peer {
            Peer::Unnamed(path) => *path,
            Peer::Host(_) => match &state.peers.get(peer)?.route {
                Route::Carrying(path) => *path,
                Route::Unset | Route::All(_) => return None,
            },
        };
        match path {
            Path::Direct(addr) | Path::Punched(addr) => Some(addr),
            Path::Forwarded(_) | Path::Framed(_) => None,
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
    /// byte stream) keeps that route, and this sender stays unnamed.
    ///
    /// A stand-in another host was last heard at, at an address, is taken
    /// from it: the address was reused (a LAN address given out again, a
    /// NAT's port given to someone else), and the host that left is
    /// forgotten until it is heard again.
    ///
    /// True when the stand-in is the host's.
    pub fn name(&self, stand_in: SocketAddr, host: HostId) -> bool {
        let mut state = self.lock();
        let (path, left) = match state.by_stand_in.get(&stand_in).copied() {
            Some(Peer::Unnamed(path)) => (path, None),
            Some(Peer::Host(named)) if named == host => return true,
            Some(Peer::Host(left)) => {
                let held = &state.peers[&Peer::Host(left)];
                match held.route {
                    Route::Carrying(path) if addressed(path) && held.stand_in == stand_in => {
                        (path, Some(left))
                    }
                    _ => return false,
                }
            }
            None => return false,
        };
        if let Some(known) = state.peers.get(&Peer::Host(host)) {
            let readdressed = match &known.route {
                Route::Unset => true,
                Route::Carrying(carrying) => addressed(*carrying) && addressed(path),
                Route::All(_) => false,
            };
            if !readdressed {
                return false;
            }
        }
        match left {
            Some(left) => state.forget(left),
            None => {
                state.peers.remove(&Peer::Unnamed(path));
            }
        }
        state
            .peers
            .entry(Peer::Host(host))
            .or_insert(PeerState {
                stand_in,
                route: Route::Unset,
            })
            .stand_in = stand_in;
        state.by_stand_in.insert(stand_in, Peer::Host(host));
        if addressed(path) {
            state.unlearn_addresses(host);
        }
        state.set_route(host, Route::Carrying(path));
        true
    }

    /// Carries `peer`'s packets on one path.
    pub fn set_carrying(&self, peer: HostId, path: Path) {
        self.lock().set_route(peer, Route::Carrying(path));
    }

    /// Sends each of `peer`'s packets on every one of `paths` until one
    /// carries the link.
    pub fn send_on_all(&self, peer: HostId, paths: Vec<Path>) {
        self.lock().set_route(peer, Route::All(paths));
    }

    /// Registers a byte-stream path; its packets arrive through
    /// [`LinkSocket::deliver_framed`].
    pub fn add_framed(&self, io: Arc<dyn FramedIo>) -> PathId {
        let mut state = self.lock();
        let id = PathId(state.next_path);
        state.next_path += 1;
        state.framed.insert(id, io);
        id
    }

    /// What a byte-stream path runs over, while it is registered.
    pub fn framed_kind(&self, path: PathId) -> Option<FramedKind> {
        Some(self.lock().framed.get(&path)?.kind())
    }

    pub fn remove_framed(&self, path: PathId) {
        let mut state = self.lock();
        state.framed.remove(&path);
        state.by_framed.remove(&path);
    }

    /// One packet arrived on a byte-stream path.
    pub fn deliver_framed(&self, path: PathId, packet: Vec<u8>) {
        let mut state = self.lock();
        if state.inbound.len() >= FRAMED_RECV_QUEUE {
            state.inbound.pop_front();
            self.drops.framed_arrivals.fetch_add(1, Ordering::Relaxed);
        }
        state.inbound.push_back((path, packet));
        if let Some(waker) = state.recv_waker.take() {
            waker.wake();
        }
    }

    /// Names the forwarder a forwarding link reaches by its stand-in on
    /// this socket: packets for a peer carried on `Path::Forwarded(link)`
    /// go behind the header on whatever path carries the forwarder, UDP or
    /// a byte stream, and packets it delivers are answered through it.
    pub fn add_forwarding(&self, link: LinkId, via: SocketAddr, kind: PathKind) {
        self.lock().forwarding.insert(link, (via, kind));
    }

    /// Forgets a forwarding link, and with it the path through it of every
    /// peer it carried: a link dialled to one of them later goes through
    /// whichever forwarding link reaches the forwarder then.
    pub fn remove_forwarding(&self, link: LinkId) {
        let mut state = self.lock();
        state.forwarding.remove(&link);
        let through = Path::Forwarded(link);
        for peer in state.peers.values_mut() {
            match &mut peer.route {
                Route::Carrying(path) if *path == through => peer.route = Route::Unset,
                Route::All(paths) => paths.retain(|path| *path != through),
                _ => {}
            }
        }
    }

    /// The forwarding link whose forwarder stands behind `stand_in`.
    pub fn forwarding_link(&self, stand_in: SocketAddr) -> Option<LinkId> {
        let state = self.lock();
        state.forwarding_of(*state.by_stand_in.get(&stand_in)?)
    }

    /// What kind of path a link carried through `link` is: through the
    /// relay or through a paired daemon.
    pub fn forwarded_kind(&self, link: LinkId) -> Option<PathKind> {
        self.lock().forwarding.get(&link).map(|(_, kind)| *kind)
    }

    /// Makes this the socket of a forwarder: a packet sent to it to
    /// forward goes to `sink`. Without one such packets are dropped.
    pub fn forward_to(&self, sink: Arc<dyn ForwardSink>) {
        self.lock().forward_sink = Some(sink);
    }

    /// The path a stand-in's packets go out on, where there is one.
    pub fn path_of(&self, stand_in: SocketAddr) -> Option<Path> {
        let state = self.lock();
        match state.by_stand_in.get(&stand_in)? {
            Peer::Unnamed(path) => Some(*path),
            peer @ Peer::Host(_) => match &state.peers.get(peer)?.route {
                Route::Carrying(path) => Some(*path),
                Route::Unset | Route::All(_) => None,
            },
        }
    }

    /// Sends a datagram as it is, not through quinn, on the path carrying
    /// the peer behind `stand_in`: how a forwarder puts a packet it
    /// forwards on the destination's link.
    pub fn send_raw(&self, stand_in: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        let (peer, path) = {
            let state = self.lock();
            let peer = *state.by_stand_in.get(&stand_in).ok_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "not a stand-in address")
            })?;
            let path = match (&peer, &state.peers[&peer].route) {
                (Peer::Unnamed(path), _) | (Peer::Host(_), Route::Carrying(path)) => *path,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "no one path carries this peer",
                    ));
                }
            };
            (peer, path)
        };
        self.send_on(peer, path, datagram)
    }

    /// Sends a probe to `peer` on `path`; its answer comes back on
    /// [`LinkSocket::probe_answers`].
    pub fn probe(&self, peer: HostId, path: Path, nonce: u64) -> io::Result<()> {
        self.send_on(Peer::Host(peer), path, &encode_probe(self.own, nonce))
    }

    /// The answers to this socket's probes from now on; a second call takes
    /// them from the first receiver.
    pub fn probe_answers(&self) -> mpsc::UnboundedReceiver<ProbeAnswered> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.lock().probe_answers = Some(sender);
        receiver
    }

    /// What the socket has dropped so far instead of failing quinn.
    pub fn drops(&self) -> Drops {
        Drops {
            sends: self.drops.sends.load(Ordering::Relaxed),
            receive_errors: self.drops.receive_errors.load(Ordering::Relaxed),
            framed_arrivals: self.drops.framed_arrivals.load(Ordering::Relaxed),
        }
    }

    fn dropped_send(&self, destination: SocketAddr, error: &io::Error) {
        self.drops.sends.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(%destination, %error, "link socket dropped a send");
    }

    /// The largest datagram every path of this peer carries.
    pub fn max_datagram(&self, _peer: HostId) -> usize {
        MAX_DATAGRAM
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send_on(&self, peer: Peer, path: Path, packet: &[u8]) -> io::Result<()> {
        match path {
            Path::Direct(addr) | Path::Punched(addr) => self.substrate().send_to(packet, addr),
            Path::Forwarded(link) => {
                let Peer::Host(host) = peer else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "an unnamed sender cannot be reached through a forwarder",
                    ));
                };
                let (forwarder, path) = {
                    let state = self.lock();
                    let (via, _) = state.forwarding.get(&link).copied().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "no such forwarding link")
                    })?;
                    let forwarder = *state.by_stand_in.get(&via).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "the forwarder is gone")
                    })?;
                    let path = match (&forwarder, &state.peers[&forwarder].route) {
                        (Peer::Unnamed(path), _) | (Peer::Host(_), Route::Carrying(path)) => *path,
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::NotConnected,
                                "no one path carries the forwarder",
                            ));
                        }
                    };
                    (forwarder, path)
                };
                // Forwarding is one hop: a forwarder is never reached
                // through another.
                if matches!(path, Path::Forwarded(_)) {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "a forwarder reached through a forwarder",
                    ));
                }
                self.send_on(forwarder, path, &encode_forwarded(host, packet))
            }
            Path::Framed(id) => {
                let io = self.lock().framed.get(&id).cloned().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "no such byte-stream path")
                })?;
                io.send(packet)
            }
        }
    }

    fn reply(&self, arrival: Arrival, packet: &[u8]) {
        let _ = match arrival {
            Arrival::Udp(addr) => self.substrate().send_to(packet, addr),
            Arrival::Framed(id) => match self.lock().framed.get(&id).cloned() {
                Some(io) => io.send(packet),
                None => Ok(()),
            },
        };
    }

    fn answered(&self, answer: ProbeAnswered) {
        let mut state = self.lock();
        if let Some(sender) = &state.probe_answers
            && sender.send(answer).is_err()
        {
            state.probe_answers = None;
        }
    }

    /// Handles one arrival in place: our own datagrams are answered or
    /// reported and dropped; a QUIC packet's stand-in and where it starts
    /// in `datagram` are returned.
    fn arrive(&self, arrival: Arrival, datagram: &[u8]) -> Option<(SocketAddr, usize)> {
        match parse(datagram).ok()? {
            PacketKind::Quic(_) => {
                let mut state = self.lock();
                let peer = state.sender(arrival);
                Some((state.stand_in(peer), 0))
            }
            PacketKind::Probe { nonce, .. } => {
                self.reply(arrival, &encode_probe_answer(self.own, nonce));
                None
            }
            PacketKind::ProbeAnswer { from, nonce } => {
                self.answered(ProbeAnswered { from, nonce });
                None
            }
            PacketKind::Forwarded { host, payload } => {
                let (sink, sender) = {
                    let mut state = self.lock();
                    let peer = state.sender(arrival);
                    (state.forward_sink.clone()?, state.stand_in(peer))
                };
                sink.arrived(sender, host, payload);
                None
            }
            PacketKind::Delivered { host, payload } => match parse(payload).ok()? {
                PacketKind::Quic(_) => {
                    let mut state = self.lock();
                    let stand_in = state.stand_in(Peer::Host(host));
                    // A link someone dialled to us through a forwarder is
                    // answered the way it came until a route is chosen.
                    if let Some(link) = state.forwarder_of(arrival) {
                        let peer = state.peers.get_mut(&Peer::Host(host)).expect("stand-in");
                        if matches!(peer.route, Route::Unset) {
                            peer.route = Route::Carrying(Path::Forwarded(link));
                        }
                    }
                    Some((stand_in, FORWARD_HEADER))
                }
                PacketKind::Probe { nonce, .. } => {
                    let answer = encode_probe_answer(self.own, nonce);
                    self.reply(arrival, &encode_forwarded(host, &answer));
                    None
                }
                PacketKind::ProbeAnswer { from, nonce } => {
                    self.answered(ProbeAnswered { from, nonce });
                    None
                }
                PacketKind::Forwarded { .. } | PacketKind::Delivered { .. } => None,
            },
        }
    }
}

impl AsyncUdpSocket for LinkSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Writable(self))
    }

    /// Only `WouldBlock` reaches quinn: it ends a connection on any other
    /// send error, and a link must outlive the path under it failing (a
    /// network going away, a forwarder gone). Any other failure is a lost
    /// packet, which QUIC recovers from or, if the path never comes back,
    /// the link's idle timeout ends.
    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        // Each transmit is one datagram sent as it is: segmentation and ECN
        // marks are not carried by every path.
        debug_assert!(transmit.segment_size.is_none(), "one datagram per transmit");
        let (peer, route) = {
            let state = self.lock();
            let Some(peer) = state.by_stand_in.get(&transmit.destination).copied() else {
                drop(state);
                self.dropped_send(
                    transmit.destination,
                    &io::Error::new(io::ErrorKind::AddrNotAvailable, "not a stand-in address"),
                );
                return Ok(());
            };
            (peer, state.peers[&peer].route.clone())
        };
        match route {
            Route::Unset => Ok(()),
            Route::Carrying(path) => match self.send_on(peer, path, transmit.contents) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(error),
                Err(error) => {
                    self.dropped_send(transmit.destination, &error);
                    Ok(())
                }
                Ok(()) => Ok(()),
            },
            Route::All(paths) => {
                // One path refusing is a lost copy; only every path being
                // full holds quinn back.
                let mut blocked = !paths.is_empty();
                for path in paths {
                    match self.send_on(peer, path, transmit.contents) {
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => {
                            self.dropped_send(transmit.destination, &error);
                            blocked = false;
                        }
                        Ok(()) => blocked = false,
                    }
                }
                if blocked {
                    Err(io::ErrorKind::WouldBlock.into())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let buf = &mut bufs[0];
        let mut errors = 0;
        loop {
            let framed = {
                let mut state = self.lock();
                if !state
                    .recv_waker
                    .as_ref()
                    .is_some_and(|waker| waker.will_wake(cx.waker()))
                {
                    state.recv_waker = Some(cx.waker().clone());
                }
                state.inbound.pop_front()
            };
            let (len, arrival) = match framed {
                Some((path, packet)) => {
                    if packet.len() > buf.len() {
                        continue;
                    }
                    buf[..packet.len()].copy_from_slice(&packet);
                    (packet.len(), Arrival::Framed(path))
                }
                None => match self.substrate().poll_recv(cx, buf) {
                    Poll::Ready(Ok((len, from))) => (len, Arrival::Udp(from)),
                    // A substrate that cannot register a wakeup: try again
                    // on the runtime's next turn.
                    Poll::Ready(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    // quinn ends the endpoint, and every link on it, on a
                    // receive error; a network change says nothing about
                    // the next datagram.
                    Poll::Ready(Err(error)) => {
                        self.drops.receive_errors.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(%error, "link socket skipped a receive error");
                        errors += 1;
                        if errors >= RECV_ERRORS_PER_POLL {
                            cx.waker().wake_by_ref();
                            return Poll::Pending;
                        }
                        continue;
                    }
                    Poll::Pending => return Poll::Pending,
                },
            };
            let Some((stand_in, start)) = self.arrive(arrival, &buf[..len]) else {
                continue;
            };
            buf.copy_within(start..len, 0);
            meta[0] = RecvMeta {
                addr: stand_in,
                len: len - start,
                stride: len - start,
                ecn: None,
                dst_ip: None,
            };
            return Poll::Ready(Ok(1));
        }
    }

    /// Every path carries one datagram at a time; `try_send` relies on it.
    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        // quinn refuses IPv6 destinations from an IPv4 socket, and every
        // stand-in is IPv6.
        let addr = self.substrate().local_addr()?;
        Ok(match addr.ip() {
            IpAddr::V4(ip) => SocketAddr::new(IpAddr::V6(ip.to_ipv6_mapped()), addr.port()),
            IpAddr::V6(_) => addr,
        })
    }
}

struct Writable(Arc<LinkSocket>);

impl fmt::Debug for Writable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Writable").finish()
    }
}

impl UdpPoller for Writable {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.0.substrate().poll_send_ready(cx)
    }
}

/// An address in its plain family: an IPv4-mapped IPv6 address as IPv4,
/// the form the OS socket reports arrivals in.
fn plain(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(ip) => SocketAddr::new(ip.into(), v6.port()),
            None => addr,
        },
        SocketAddr::V4(_) => addr,
    }
}

fn bind_udp(bind: SocketAddr, remembered_port: Option<u16>) -> io::Result<std::net::UdpSocket> {
    if bind.port() == 0
        && let Some(port) = remembered_port.filter(|port| *port != 0)
        && let Ok(socket) = bind_one(bind.ip(), port)
    {
        return Ok(socket);
    }
    bind_one(bind.ip(), bind.port())
}

fn bind_one(ip: IpAddr, port: u16) -> io::Result<std::net::UdpSocket> {
    if !ip.is_unspecified() {
        return std::net::UdpSocket::bind(SocketAddr::new(ip, port));
    }
    match dual_stack_socket() {
        Ok(socket) => {
            socket.bind(&SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port).into())?;
            Ok(socket.into())
        }
        // No IPv6 on this host: IPv4 alone.
        Err(_) => std::net::UdpSocket::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port)),
    }
}

fn dual_stack_socket() -> io::Result<socket2::Socket> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_only_v6(false)?;
    Ok(socket)
}

/// The OS socket. Addresses cross it in their plain family: an IPv4 peer
/// on the dual-stack socket reads as IPv4, not as an IPv4-mapped IPv6
/// address, so a path learned in one form matches arrivals in the other.
struct Udp {
    socket: tokio::net::UdpSocket,
    v6: bool,
}

impl Udp {
    fn new(socket: tokio::net::UdpSocket) -> io::Result<Self> {
        let v6 = socket.local_addr()?.is_ipv6();
        Ok(Self { socket, v6 })
    }
}

impl Datagrams for Udp {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()> {
        let to = match (self.v6, to.ip()) {
            (true, IpAddr::V4(ip)) => SocketAddr::new(ip.to_ipv6_mapped().into(), to.port()),
            (false, IpAddr::V6(ip)) => match ip.to_ipv4_mapped() {
                Some(ip) => SocketAddr::new(ip.into(), to.port()),
                None => to,
            },
            _ => to,
        };
        self.socket.try_send_to(buf, to).map(|_| ())
    }

    fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.socket.poll_send_ready(cx)
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        loop {
            let mut read = tokio::io::ReadBuf::new(buf);
            match self.socket.poll_recv_from(cx, &mut read) {
                Poll::Ready(Ok(from)) => {
                    // plain keeps a link-local sender's scope, which the
                    // answer needs to find its interface.
                    return Poll::Ready(Ok((read.filled().len(), plain(from))));
                }
                // Windows reports an earlier send's ICMP unreachable on the
                // next receive; it says nothing about this one.
                Poll::Ready(Err(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::trust::{SharedTrustStore, TrustStore, TrustStorePairingUpdate};

    /// An in-memory substrate: what was sent, and what to receive next.
    #[derive(Default)]
    struct Fake {
        sent: Mutex<Vec<(SocketAddr, Vec<u8>)>>,
        incoming: Mutex<VecDeque<(SocketAddr, Vec<u8>)>>,
        blocked: Mutex<Vec<SocketAddr>>,
        /// Addresses every send to fails, as on a network that went away.
        unreachable: Mutex<Vec<SocketAddr>>,
        /// Receive errors to report before the next datagram.
        recv_errors: Mutex<usize>,
    }

    impl Fake {
        fn push(&self, from: SocketAddr, datagram: &[u8]) {
            self.incoming
                .lock()
                .unwrap()
                .push_back((from, datagram.to_vec()));
        }

        fn take_sent(&self) -> Vec<(SocketAddr, Vec<u8>)> {
            std::mem::take(&mut self.sent.lock().unwrap())
        }
    }

    impl Datagrams for Fake {
        fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()> {
            if self.blocked.lock().unwrap().contains(&to) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            if self.unreachable.lock().unwrap().contains(&to) {
                return Err(io::ErrorKind::NetworkUnreachable.into());
            }
            self.sent.lock().unwrap().push((to, buf.to_vec()));
            Ok(())
        }

        fn poll_recv(
            &self,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<(usize, SocketAddr)>> {
            let mut errors = self.recv_errors.lock().unwrap();
            if *errors > 0 {
                *errors -= 1;
                return Poll::Ready(Err(io::ErrorKind::NetworkDown.into()));
            }
            drop(errors);
            match self.incoming.lock().unwrap().pop_front() {
                Some((from, datagram)) => {
                    buf[..datagram.len()].copy_from_slice(&datagram);
                    Poll::Ready(Ok((datagram.len(), from)))
                }
                None => Poll::Pending,
            }
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok("192.0.2.1:4000".parse().unwrap())
        }
    }

    fn fake_socket() -> (Arc<Fake>, Arc<LinkSocket>, HostId) {
        let fake = Arc::new(Fake::default());
        let own = HostId::new_v4();
        let socket = LinkSocket::bind(fake.clone(), own).unwrap();
        (fake, socket, own)
    }

    fn transmit(destination: SocketAddr, contents: &[u8]) -> Transmit<'_> {
        Transmit {
            destination,
            ecn: None,
            contents,
            segment_size: None,
            src_ip: None,
        }
    }

    /// One poll of quinn's receive side: the stand-in and the packet, or
    /// nothing if the socket kept everything to itself.
    fn receive(socket: &LinkSocket) -> Option<(SocketAddr, Vec<u8>)> {
        let mut storage = [0u8; 2048];
        let mut bufs = [IoSliceMut::new(&mut storage)];
        let mut meta = [RecvMeta::default()];
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match socket.poll_recv(&mut cx, &mut bufs, &mut meta) {
            Poll::Ready(Ok(1)) => Some((meta[0].addr, bufs[0][..meta[0].len].to_vec())),
            Poll::Pending => None,
            other => panic!("unexpected receive: {other:?}"),
        }
    }

    /// A short-header QUIC packet: the fixed bit set.
    const QUIC: &[u8] = &[0x40, 1, 2, 3, 4];

    #[test]
    fn socket_parses_each_kind_by_the_fixed_bit() {
        let host = HostId::new_v4();
        assert_eq!(parse(QUIC), Ok(PacketKind::Quic(QUIC)));
        // A long header has the fixed bit too.
        let long = [0xc0, 0, 0, 0, 1];
        assert_eq!(parse(&long), Ok(PacketKind::Quic(&long)));

        let forwarded = encode_forwarded(host, QUIC);
        assert_eq!(forwarded.len(), FORWARD_HEADER + QUIC.len());
        assert_eq!(
            parse(&forwarded),
            Ok(PacketKind::Forwarded {
                host,
                payload: QUIC
            })
        );
        let delivered = encode_delivered(host, QUIC);
        assert_eq!(delivered.len(), FORWARD_HEADER + QUIC.len());
        assert_eq!(
            parse(&delivered),
            Ok(PacketKind::Delivered {
                host,
                payload: QUIC
            })
        );
        assert_eq!(
            parse(&encode_probe(host, 7)),
            Ok(PacketKind::Probe {
                from: host,
                nonce: 7
            })
        );
        assert_eq!(
            parse(&encode_probe_answer(host, u64::MAX)),
            Ok(PacketKind::ProbeAnswer {
                from: host,
                nonce: u64::MAX
            })
        );

        assert_eq!(parse(&[]), Err(Malformed::Empty));
        assert_eq!(parse(&[0x04, 0]), Err(Malformed::UnknownKind(0x04)));
        // A header with nothing behind it, and probes of the wrong length.
        assert_eq!(parse(&forwarded[..FORWARD_HEADER]), Err(Malformed::Length));
        assert_eq!(
            parse(&encode_probe(host, 7)[..PROBE_LEN - 1]),
            Err(Malformed::Length)
        );
        let mut long_probe = encode_probe(host, 7).to_vec();
        long_probe.push(0);
        assert_eq!(parse(&long_probe), Err(Malformed::Length));
    }

    #[test]
    fn socket_endpoints_never_offer_to_grease_the_fixed_bit() {
        let config = format!("{:?}", crate::identity::quic_endpoint_config());
        assert!(config.contains("grease_quic_bit: false"), "{config}");
    }

    #[test]
    fn socket_largest_datagram_fits_ipv6_behind_the_header() {
        let (_fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        assert_eq!(socket.max_datagram(peer), 1200);
        // IPv6's minimum MTU less its own and UDP's headers.
        assert!(socket.max_datagram(peer) + FORWARD_HEADER <= 1280 - 40 - 8);
        // The fake reports a v4 address; quinn still sees an IPv6 socket.
        assert!(AsyncUdpSocket::local_addr(&*socket).unwrap().is_ipv6());
    }

    /// A forwarding link that goes takes the paths through it: the peer it
    /// carried has none until a link through the forwarder's next one sets
    /// it, so a dial then is not sent into the old one.
    #[test]
    fn removing_a_forwarding_link_unsets_the_paths_through_it() {
        let (fake, socket, _own) = fake_socket();
        let (a, b) = (HostId::new_v4(), HostId::new_v4());
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(9), via, PathKind::RelayUdp);
        socket.set_carrying(a, Path::Forwarded(LinkId(9)));
        socket.set_carrying(b, Path::Direct(lan));
        let (stand_in_a, stand_in_b) = (socket.stand_in(a), socket.stand_in(b));

        socket.remove_forwarding(LinkId(9));
        assert_eq!(socket.path_of(stand_in_a), None);
        assert_eq!(socket.path_of(stand_in_b), Some(Path::Direct(lan)));

        socket.add_forwarding(LinkId(10), via, PathKind::RelayUdp);
        socket.set_carrying(a, Path::Forwarded(LinkId(10)));
        socket.try_send(&transmit(stand_in_a, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(relay, encode_forwarded(a, QUIC))]);
    }

    #[test]
    fn socket_stand_ins_map_both_ways() {
        let (fake, socket, _own) = fake_socket();
        let (a, b) = (HostId::new_v4(), HostId::new_v4());
        let stand_in_a = socket.stand_in(a);
        assert_eq!(socket.stand_in(a), stand_in_a, "a stand-in is stable");
        assert_ne!(socket.stand_in(b), stand_in_a);
        assert_eq!(socket.peer_of(stand_in_a), Some(a));

        // Outward: quinn's packet to a's stand-in goes to a's path.
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        socket.set_carrying(a, Path::Direct(lan));
        socket.try_send(&transmit(stand_in_a, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(lan, QUIC.to_vec())]);

        // Inward: a's packet from that path comes back from the stand-in.
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket), Some((stand_in_a, QUIC.to_vec())));

        // Through a forwarder: the header names the destination going out
        // and the sender coming in, and quinn sees neither.
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(9), via, PathKind::RelayUdp);
        socket.set_carrying(a, Path::Forwarded(LinkId(9)));
        socket.try_send(&transmit(stand_in_a, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(relay, encode_forwarded(a, QUIC))]);
        fake.push(relay, &encode_delivered(a, QUIC));
        assert_eq!(receive(&socket), Some((stand_in_a, QUIC.to_vec())));
        // Still the same stand-in on the old path: the switch is invisible.
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket), Some((stand_in_a, QUIC.to_vec())));

        // A byte-stream path maps the same way.
        let framed = Arc::new(Recorded::default());
        let path = socket.add_framed(framed.clone());
        socket.set_carrying(b, Path::Framed(path));
        socket
            .try_send(&transmit(socket.stand_in(b), QUIC))
            .unwrap();
        assert_eq!(framed.sent.lock().unwrap().as_slice(), [QUIC.to_vec()]);
        socket.deliver_framed(path, QUIC.to_vec());
        assert_eq!(receive(&socket), Some((socket.stand_in(b), QUIC.to_vec())));

        // An address that is not a stand-in goes nowhere, as a lost packet.
        socket.try_send(&transmit(lan, QUIC)).unwrap();
        assert!(fake.take_sent().is_empty());
        assert_eq!(socket.drops().sends, 1);
    }

    /// quinn ends a connection on any send error but `WouldBlock`, so a
    /// failing path loses the packet instead and the link lives on.
    #[test]
    fn socket_failed_sends_are_lost_packets() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let stand_in = socket.stand_in(peer);
        socket.set_carrying(peer, Path::Direct(lan));

        fake.unreachable.lock().unwrap().push(lan);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(socket.drops().sends, 1);
        // Through a forwarding link that is gone.
        socket.set_carrying(peer, Path::Forwarded(LinkId(5)));
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(socket.drops().sends, 2);
        // On a byte stream that has ended.
        let path = socket.add_framed(Arc::new(Recorded::default()));
        socket.remove_framed(path);
        socket.set_carrying(peer, Path::Framed(path));
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(socket.drops().sends, 3);
        assert!(fake.take_sent().is_empty());

        // A full substrate still holds quinn back.
        socket.set_carrying(peer, Path::Direct(lan));
        fake.unreachable.lock().unwrap().clear();
        fake.blocked.lock().unwrap().push(lan);
        let error = socket.try_send(&transmit(stand_in, QUIC)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(socket.drops().sends, 3);
    }

    #[test]
    fn socket_skips_receive_errors() {
        let (fake, socket, _own) = fake_socket();
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        // More errors than one poll skips: that poll yields, the next goes on.
        *fake.recv_errors.lock().unwrap() = RECV_ERRORS_PER_POLL + 2;
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket), None);
        let (_, packet) = receive(&socket).expect("the datagram behind the errors");
        assert_eq!(packet, QUIC);
        assert_eq!(
            socket.drops().receive_errors,
            RECV_ERRORS_PER_POLL as u64 + 2
        );
    }

    #[test]
    fn socket_framed_receive_queue_drops_the_oldest() {
        let (_fake, socket, _own) = fake_socket();
        let path = socket.add_framed(Arc::new(Recorded::default()));
        let packet = |i: usize| [0x40, (i >> 8) as u8, i as u8].to_vec();
        for i in 0..FRAMED_RECV_QUEUE + 3 {
            socket.deliver_framed(path, packet(i));
        }
        assert_eq!(socket.drops().framed_arrivals, 3);
        let mut received = Vec::new();
        while let Some((_, got)) = receive(&socket) {
            received.push(got);
        }
        assert_eq!(received.len(), FRAMED_RECV_QUEUE);
        assert_eq!(received[0], packet(3));
        assert_eq!(received.last(), Some(&packet(FRAMED_RECV_QUEUE + 2)));
    }

    #[test]
    fn socket_transport_keeps_every_datagram_at_the_one_size() {
        let config = format!("{:?}", crate::identity::quic_transport_config());
        for field in [
            "initial_mtu: 1200",
            "min_mtu: 1200",
            "mtu_discovery_config: None",
        ] {
            assert!(config.contains(field), "{field} in {config}");
        }
    }

    #[test]
    fn socket_unnamed_sender_keeps_its_stand_in_once_named() {
        let (fake, socket, _own) = fake_socket();
        let dialler: SocketAddr = "192.0.2.20:6000".parse().unwrap();
        fake.push(dialler, QUIC);
        let (stand_in, _) = receive(&socket).unwrap();
        assert_eq!(socket.peer_of(stand_in), None);
        // The reply goes back the way it came.
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(dialler, QUIC.to_vec())]);

        let host = HostId::new_v4();
        assert!(socket.name(stand_in, host));
        assert_eq!(socket.stand_in(host), stand_in);
        assert_eq!(socket.peer_of(stand_in), Some(host));
        fake.push(dialler, QUIC);
        assert_eq!(receive(&socket).unwrap().0, stand_in);
        assert!(
            socket.name(stand_in, host),
            "naming it again changes nothing"
        );
        assert_eq!(socket.peer_of(stand_in), Some(host));
    }

    /// An address is one host's at a time. A host proving itself at the
    /// stand-in another was last heard at (a reused LAN address) takes it
    /// and the host that left is forgotten; a host given an address
    /// another was carried on takes it from that host's route.
    #[test]
    fn socket_reused_address_never_maps_to_the_host_that_left_it() {
        let (fake, socket, _own) = fake_socket();
        let lan: SocketAddr = "192.0.2.40:5000".parse().unwrap();
        let (laptop, desk) = (HostId::new_v4(), HostId::new_v4());
        fake.push(lan, QUIC);
        let (at_lan, _) = receive(&socket).unwrap();
        assert!(socket.name(at_lan, laptop));

        // The laptop left; the desk is given its address and connects
        // from it. Its packets come in at the laptop's stand-in until its
        // handshake names it there.
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket).unwrap().0, at_lan);
        assert!(socket.name(at_lan, desk));
        assert_eq!(socket.peer_of(at_lan), Some(desk));
        assert_ne!(socket.stand_in(laptop), at_lan, "the laptop is forgotten");
        assert_eq!(socket.path_of(socket.stand_in(laptop)), None);
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket).unwrap().0, at_lan);

        // The laptop is carried at the address: the desk loses it.
        socket.set_carrying(laptop, Path::Direct(lan));
        assert_eq!(socket.path_of(at_lan), None);
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket).unwrap().0, socket.stand_in(laptop));

        // A path the laptop switches from still leads to it, for the
        // packets still in flight there.
        let elsewhere: SocketAddr = "192.0.2.41:5000".parse().unwrap();
        socket.set_carrying(laptop, Path::Direct(elsewhere));
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket).unwrap().0, socket.stand_in(laptop));
    }

    /// A host heard again at a new address (its connection moved there,
    /// or it connected again from there) is named afresh: its packets map
    /// to the new stand-in, its earlier stand-in reaches it at the new
    /// address, and the old address is let go. A host carried through a
    /// forwarder keeps that route.
    #[test]
    fn socket_names_a_host_afresh_at_a_new_address() {
        let (fake, socket, _own) = fake_socket();
        let (old, new): (SocketAddr, SocketAddr) = (
            "192.0.2.50:5000".parse().unwrap(),
            "203.0.113.7:6100".parse().unwrap(),
        );
        let phone = HostId::new_v4();
        fake.push(old, QUIC);
        let (first, _) = receive(&socket).unwrap();
        assert!(socket.name(first, phone));
        fake.push(new, QUIC);
        let (moved, _) = receive(&socket).unwrap();
        assert_ne!(moved, first);
        assert!(socket.name(moved, phone));
        assert_eq!(socket.stand_in(phone), moved);
        assert_eq!(socket.peer_of(first), Some(phone));
        for stand_in in [first, moved] {
            assert_eq!(socket.address_of(stand_in), Some(new));
            socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        }
        assert_eq!(
            fake.take_sent(),
            vec![(new, QUIC.to_vec()), (new, QUIC.to_vec())]
        );
        fake.push(new, QUIC);
        assert_eq!(receive(&socket).unwrap().0, moved);
        fake.push(old, QUIC);
        assert_eq!(socket.peer_of(receive(&socket).unwrap().0), None);

        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(5), via, PathKind::RelayUdp);
        let desk = HostId::new_v4();
        socket.set_carrying(desk, Path::Forwarded(LinkId(5)));
        fake.push("192.0.2.60:5000".parse().unwrap(), QUIC);
        let (direct, _) = receive(&socket).unwrap();
        assert!(!socket.name(direct, desk));
        assert_eq!(
            socket.path_of(socket.stand_in(desk)),
            Some(Path::Forwarded(LinkId(5)))
        );
    }

    #[test]
    fn socket_forwarded_dial_is_answered_through_its_forwarder() {
        let (fake, socket, _own) = fake_socket();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(3), via, PathKind::RelayUdp);
        let peer = HostId::new_v4();
        fake.push(relay, &encode_delivered(peer, QUIC));
        let (stand_in, packet) = receive(&socket).unwrap();
        assert_eq!(
            (socket.peer_of(stand_in), packet),
            (Some(peer), QUIC.to_vec())
        );
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(relay, encode_forwarded(peer, QUIC))]
        );
    }

    /// A forwarder's sink that keeps what it is handed.
    #[derive(Default)]
    struct Kept(Mutex<Vec<(SocketAddr, HostId, Vec<u8>)>>);

    impl ForwardSink for Kept {
        fn arrived(&self, sender: SocketAddr, dst: HostId, payload: &[u8]) {
            self.0.lock().unwrap().push((sender, dst, payload.to_vec()));
        }
    }

    #[test]
    fn socket_of_a_daemon_forwards_for_a_host_it_also_sends_through() {
        let (fake, socket, _own) = fake_socket();
        let kept = Arc::new(Kept::default());
        socket.forward_to(kept.clone());
        let laptop: SocketAddr = "192.0.2.30:7000".parse().unwrap();
        let via = socket.stand_in_at(laptop);
        socket.add_forwarding(LinkId(4), via, PathKind::ViaDaemon);
        let (desk, phone) = (HostId::new_v4(), HostId::new_v4());

        // Sent to forward: to the sink, though the link is one this socket
        // sends through.
        fake.push(laptop, &encode_forwarded(desk, QUIC));
        assert_eq!(receive(&socket), None);
        assert_eq!(
            kept.0.lock().unwrap().as_slice(),
            [(via, desk, QUIC.to_vec())]
        );

        // Delivered: this socket's own, from the host it names.
        fake.push(laptop, &encode_delivered(phone, QUIC));
        let (stand_in, packet) = receive(&socket).unwrap();
        assert_eq!(
            (socket.peer_of(stand_in), packet),
            (Some(phone), QUIC.to_vec())
        );
        assert_eq!(kept.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn socket_sends_on_all_until_one_carries() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let public: SocketAddr = "203.0.113.5:5000".parse().unwrap();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(1), via, PathKind::RelayUdp);
        let stand_in = socket.stand_in(peer);

        // No route yet: dropped as a lost packet would be.
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert!(fake.take_sent().is_empty());

        socket.send_on_all(
            peer,
            vec![
                Path::Direct(lan),
                Path::Punched(public),
                Path::Forwarded(LinkId(1)),
            ],
        );
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![
                (lan, QUIC.to_vec()),
                (public, QUIC.to_vec()),
                (relay, encode_forwarded(peer, QUIC)),
            ]
        );
        // Answers on any of them reach the one stand-in.
        for from in [lan, public] {
            fake.push(from, QUIC);
            assert_eq!(receive(&socket).unwrap().0, stand_in);
        }

        // One full path is a lost copy; every path full holds quinn back.
        fake.blocked.lock().unwrap().push(lan);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent().len(), 2);
        fake.blocked.lock().unwrap().extend([public, relay]);
        let error = socket.try_send(&transmit(stand_in, QUIC)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        fake.blocked.lock().unwrap().clear();

        // Once one carries, only it does.
        socket.set_carrying(peer, Path::Punched(public));
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(public, QUIC.to_vec())]);
    }

    #[test]
    fn socket_answers_probes_and_reports_answers() {
        let (fake, socket, own) = fake_socket();
        let mut answers = socket.probe_answers();
        let peer = HostId::new_v4();
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(1), via, PathKind::RelayUdp);

        socket.probe(peer, Path::Direct(lan), 11).unwrap();
        socket.probe(peer, Path::Forwarded(LinkId(1)), 12).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![
                (lan, encode_probe(own, 11).to_vec()),
                (relay, encode_forwarded(peer, &encode_probe(own, 12))),
            ]
        );

        // A probe is answered on the path it came by and never reaches quinn.
        fake.push(lan, &encode_probe(peer, 21));
        fake.push(relay, &encode_delivered(peer, &encode_probe(peer, 22)));
        assert_eq!(receive(&socket), None);
        assert_eq!(
            fake.take_sent(),
            vec![
                (lan, encode_probe_answer(own, 21).to_vec()),
                (relay, encode_forwarded(peer, &encode_probe_answer(own, 22))),
            ]
        );

        // Answers are reported, directly or through the forwarder.
        fake.push(lan, &encode_probe_answer(peer, 11));
        fake.push(
            relay,
            &encode_delivered(peer, &encode_probe_answer(peer, 12)),
        );
        // Malformed datagrams are dropped on the way.
        fake.push(lan, &[0x07]);
        assert_eq!(receive(&socket), None);
        assert_eq!(
            answers.try_recv().unwrap(),
            ProbeAnswered {
                from: peer,
                nonce: 11
            }
        );
        assert_eq!(
            answers.try_recv().unwrap(),
            ProbeAnswered {
                from: peer,
                nonce: 12
            }
        );
    }

    #[derive(Default)]
    struct Recorded {
        sent: Mutex<Vec<Vec<u8>>>,
    }

    impl FramedIo for Recorded {
        fn send(&self, packet: &[u8]) -> io::Result<()> {
            self.sent.lock().unwrap().push(packet.to_vec());
            Ok(())
        }

        fn kind(&self) -> FramedKind {
            FramedKind::InProcess
        }
    }

    #[tokio::test]
    async fn socket_dual_stack_bind_hears_both_families() {
        let substrate = LinkSocket::bind_dual_stack("0.0.0.0:0".parse().unwrap(), None).unwrap();
        let local = substrate.local_addr().unwrap();
        let port = local.port();
        let v4 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // A host without IPv6 binds the IPv4 fallback.
        let v6 = tokio::net::UdpSocket::bind("[::1]:0").await.ok();
        if v6.is_some() {
            assert_eq!(local.ip(), IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        }

        v4.send_to(b"\x40v4", ("127.0.0.1", port)).await.unwrap();
        let (len, from) = recv(&*substrate).await;
        // An IPv4 sender reads as IPv4, not as an IPv4-mapped address.
        assert_eq!((len, from), (3, v4.local_addr().unwrap()));
        substrate.send_to(b"back", from).unwrap();
        let mut buf = [0; 16];
        let (len, _) = v4.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"back");

        if let Some(v6) = v6 {
            v6.send_to(b"\x40v6", ("::1", port)).await.unwrap();
            let (_, from) = recv(&*substrate).await;
            assert_eq!(from, v6.local_addr().unwrap());
        }
    }

    #[tokio::test]
    async fn socket_bind_reuses_the_remembered_port() {
        let first = LinkSocket::bind_dual_stack("0.0.0.0:0".parse().unwrap(), None).unwrap();
        let port = first.local_addr().unwrap().port();
        drop(first);
        let again = LinkSocket::bind_dual_stack("0.0.0.0:0".parse().unwrap(), Some(port)).unwrap();
        assert_eq!(again.local_addr().unwrap().port(), port);
        // Taken: any port rather than none.
        let other = LinkSocket::bind_dual_stack("0.0.0.0:0".parse().unwrap(), Some(port)).unwrap();
        assert_ne!(other.local_addr().unwrap().port(), port);
        // An explicit port is bound as given.
        let loopback =
            LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), Some(port)).unwrap();
        assert_eq!(loopback.local_addr().unwrap().ip(), Ipv4Addr::LOCALHOST);
    }

    async fn recv(substrate: &dyn Datagrams) -> (usize, SocketAddr) {
        let mut buf = [0; 64];
        tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| substrate.poll_recv(cx, &mut buf)),
        )
        .await
        .expect("datagram arrives")
        .unwrap()
    }

    fn trust_for(peer: &DeviceIdentity) -> SharedTrustStore {
        let mut trust = TrustStore::default();
        assert_eq!(
            trust
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "peer".to_string(),
                    None,
                    chrono::Utc::now(),
                )
                .unwrap(),
            TrustStorePairingUpdate::Inserted
        );
        Arc::new(std::sync::RwLock::new(trust))
    }

    fn endpoint(socket: Arc<LinkSocket>, server: Option<quinn::ServerConfig>) -> quinn::Endpoint {
        quinn::Endpoint::new_with_abstract_socket(
            crate::identity::quic_endpoint_config(),
            server,
            socket,
            quinn::default_runtime().unwrap(),
        )
        .unwrap()
    }

    /// Two hosts' link sockets over loopback: a QUIC connection between
    /// stand-ins, its first flight sent on a dead address and the live one
    /// at once, then carried on the live one alone.
    #[tokio::test]
    async fn socket_carries_a_quic_connection_sent_on_all() {
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_socket = LinkSocket::bind(
            LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), None).unwrap(),
            server_identity.host_id,
        )
        .unwrap();
        let client_socket = LinkSocket::bind(
            LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), None).unwrap(),
            client_identity.host_id,
        )
        .unwrap();
        let server_addr = server_socket.local_addr().unwrap();
        let server_endpoint = endpoint(
            server_socket.clone(),
            Some(
                server_identity
                    .quic_server_config(trust_for(&client_identity))
                    .unwrap(),
            ),
        );
        let client_endpoint = endpoint(client_socket.clone(), None);

        let dead = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        client_socket.send_on_all(
            server_identity.host_id,
            vec![Path::Direct(dead_addr), Path::Direct(server_addr)],
        );
        let config = client_identity
            .quic_client_config_for_peer(trust_for(&server_identity), server_identity.host_id)
            .unwrap();
        let stand_in = client_socket.stand_in(server_identity.host_id);
        let connect = async {
            client_endpoint
                .connect_with(config, stand_in, crate::link::quic::DEVICE_SERVER_NAME)
                .unwrap()
                .await
                .unwrap()
        };
        let accept = async { server_endpoint.accept().await.unwrap().await.unwrap() };
        let (client, server) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(connect, accept)
        })
        .await
        .expect("handshake completes");
        // The dead address got the same first flight.
        let mut buf = [0; 2048];
        let (len, _) = dead.recv_from(&mut buf).await.unwrap();
        assert!(matches!(parse(&buf[..len]), Ok(PacketKind::Quic(_))));

        // The server heard an unnamed dialler; the handshake names it.
        let dialler = server.remote_address();
        assert_eq!(server_socket.peer_of(dialler), None);
        assert!(server_socket.name(dialler, client_identity.host_id));
        assert_eq!(server_socket.stand_in(client_identity.host_id), dialler);
        assert_eq!(client.remote_address(), stand_in);

        client_socket.set_carrying(server_identity.host_id, Path::Direct(server_addr));
        let echo = async {
            let (mut send, mut recv) = server.accept_bi().await.unwrap();
            let got = recv.read_to_end(64).await.unwrap();
            send.write_all(&got).await.unwrap();
            send.finish().unwrap();
            let _ = send.stopped().await;
        };
        let ask = async {
            let (mut send, mut recv) = client.open_bi().await.unwrap();
            send.write_all(b"over the stand-in").await.unwrap();
            send.finish().unwrap();
            recv.read_to_end(64).await.unwrap()
        };
        let ((), reply) =
            tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(echo, ask) })
                .await
                .expect("stream round trip");
        assert_eq!(reply, b"over the stand-in");
        assert_eq!(server.remote_address(), dialler);
    }

    /// A real UDP substrate that can be made to fail, as a network change
    /// fails it.
    struct Flaky {
        inner: Arc<dyn Datagrams>,
        refuse_sends: std::sync::atomic::AtomicBool,
        recv_errors: std::sync::atomic::AtomicUsize,
    }

    impl Flaky {
        fn loopback() -> Arc<Self> {
            Arc::new(Self {
                inner: LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), None).unwrap(),
                refuse_sends: Default::default(),
                recv_errors: Default::default(),
            })
        }
    }

    impl Datagrams for Flaky {
        fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()> {
            if self.refuse_sends.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::NetworkUnreachable.into());
            }
            self.inner.send_to(buf, to)
        }

        fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.inner.poll_send_ready(cx)
        }

        fn poll_recv(
            &self,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<(usize, SocketAddr)>> {
            let take = |left: usize| left.checked_sub(1);
            if self
                .recv_errors
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, take)
                .is_ok()
            {
                return Poll::Ready(Err(io::ErrorKind::NetworkDown.into()));
            }
            self.inner.poll_recv(cx, buf)
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }
    }

    /// A link between two hosts' sockets over loopback on flaky
    /// substrates, carried on the server's address.
    struct Loopback {
        client_net: Arc<Flaky>,
        server_net: Arc<Flaky>,
        client_socket: Arc<LinkSocket>,
        server_socket: Arc<LinkSocket>,
        client: quinn::Connection,
        server: quinn::Connection,
        client_endpoint: quinn::Endpoint,
        _server_endpoint: quinn::Endpoint,
    }

    async fn loopback(transport: Arc<quinn::TransportConfig>) -> Loopback {
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let (client_net, server_net) = (Flaky::loopback(), Flaky::loopback());
        let server_socket = LinkSocket::bind(server_net.clone(), server_identity.host_id).unwrap();
        let client_socket = LinkSocket::bind(client_net.clone(), client_identity.host_id).unwrap();
        let mut server_config = server_identity
            .quic_server_config(trust_for(&client_identity))
            .unwrap();
        server_config.transport_config(transport.clone());
        let server_endpoint = endpoint(server_socket.clone(), Some(server_config));
        let client_endpoint = endpoint(client_socket.clone(), None);
        let server_addr = server_socket.local_addr().unwrap();
        client_socket.set_carrying(server_identity.host_id, Path::Direct(server_addr));
        let mut config = client_identity
            .quic_client_config_for_peer(trust_for(&server_identity), server_identity.host_id)
            .unwrap();
        config.transport_config(transport);
        let stand_in = client_socket.stand_in(server_identity.host_id);
        let connect = async {
            client_endpoint
                .connect_with(config, stand_in, crate::link::quic::DEVICE_SERVER_NAME)
                .unwrap()
                .await
                .unwrap()
        };
        let accept = async { server_endpoint.accept().await.unwrap().await.unwrap() };
        let (client, server) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(connect, accept)
        })
        .await
        .expect("handshake completes");
        Loopback {
            client_net,
            server_net,
            client_socket,
            server_socket,
            client,
            server,
            client_endpoint,
            _server_endpoint: server_endpoint,
        }
    }

    async fn round_trip(link: &Loopback, message: &[u8]) -> Vec<u8> {
        let echo = async {
            let (mut send, mut recv) = link.server.accept_bi().await.unwrap();
            let got = recv.read_to_end(64).await.unwrap();
            send.write_all(&got).await.unwrap();
            send.finish().unwrap();
            let _ = send.stopped().await;
        };
        let ask = async {
            let (mut send, mut recv) = link.client.open_bi().await.unwrap();
            send.write_all(message).await.unwrap();
            send.finish().unwrap();
            recv.read_to_end(64).await.unwrap()
        };
        let ((), reply) =
            tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(echo, ask) })
                .await
                .expect("stream round trip");
        reply
    }

    /// A link whose carrying path refuses every send stays up, and ends
    /// only by its idle timeout, not by quinn giving up on the error.
    #[tokio::test]
    async fn socket_link_outlives_a_path_refusing_every_send() {
        const IDLE: Duration = Duration::from_secs(2);
        let mut transport = crate::identity::quic_transport_base();
        transport
            .keep_alive_interval(Some(Duration::from_millis(300)))
            .max_idle_timeout(Some(IDLE.try_into().unwrap()));
        let link = loopback(Arc::new(transport)).await;
        assert_eq!(round_trip(&link, b"before").await, b"before");

        link.client_net.refuse_sends.store(true, Ordering::SeqCst);
        let refused = tokio::time::Instant::now();
        tokio::time::sleep(IDLE / 2).await;
        assert!(link.client_socket.drops().sends > 0, "keepalives were sent");
        assert_eq!(link.client.close_reason(), None, "a refused send is a loss");

        let reason = tokio::time::timeout(Duration::from_secs(15), link.client.closed())
            .await
            .expect("the idle timeout ends the link");
        assert_eq!(reason, quinn::ConnectionError::TimedOut);
        assert!(refused.elapsed() >= IDLE);
    }

    /// The dialler's network changes under a link it holds: quinn moves
    /// the accepting side's connection to the stand-in the dialler's new
    /// address maps to, packets go on both ways, and naming the dialler
    /// there carries it at the new address under either stand-in.
    #[tokio::test]
    async fn socket_accepting_side_keeps_a_peer_that_rebinds() {
        let link = loopback(crate::identity::quic_transport_config()).await;
        let dialler = link.client_socket.own();
        let first = link.server.remote_address();
        assert!(link.server_socket.name(first, dialler));
        assert_eq!(round_trip(&link, b"before").await, b"before");

        let elsewhere = Flaky::loopback();
        let new_addr = elsewhere.local_addr().unwrap();
        link.client_socket.rebind(elsewhere);
        link.client_endpoint
            .rebind_abstract(link.client_socket.clone())
            .unwrap();
        assert_eq!(round_trip(&link, b"after").await, b"after");
        let moved = link.server.remote_address();
        assert_ne!(moved, first, "quinn moved the connection");
        assert_eq!(link.server_socket.address_of(moved), Some(new_addr));

        assert!(link.server_socket.name(moved, dialler));
        assert_eq!(link.server_socket.stand_in(dialler), moved);
        assert_eq!(link.server_socket.address_of(first), Some(new_addr));
        assert_eq!(round_trip(&link, b"named again").await, b"named again");
        assert_eq!(link.server.remote_address(), moved);
        assert_eq!(link.server.close_reason(), None);
    }

    /// A receive error is skipped: the endpoint goes on driving its links.
    #[tokio::test]
    async fn socket_receive_error_leaves_the_endpoint_running() {
        let link = loopback(crate::identity::quic_transport_config()).await;
        link.server_net.recv_errors.store(5, Ordering::SeqCst);
        assert_eq!(round_trip(&link, b"after errors").await, b"after errors");
        assert_eq!(link.server_socket.drops().receive_errors, 5);
        assert_eq!(link.server.close_reason(), None);
    }
}
