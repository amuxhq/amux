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
use socket_core::{Again, Arrival, Heard, NoPath, Peer, Peers, Transmit as Sends, Wire, WireTo};
pub use socket_core::{
    FORWARD_HEADER, MAX_DATAGRAM, Malformed, PacketKind, encode_delivered, encode_forwarded,
    encode_probe, encode_probe_answer, parse,
};
use tokio::sync::mpsc;
use tokio::time::Instant;

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
    /// Binds afresh where it was bound, after the network changed under
    /// it: true when it did, false when it is not the socket's to bind.
    fn rebind(&self) -> io::Result<bool> {
        Ok(false)
    }
}

/// Whether a QUIC packet is an Initial: one of the first a handshake
/// sends, either way.
fn handshake(packet: &[u8]) -> bool {
    // A long header with the fixed bit set and the Initial type (QUIC v1).
    packet.first().is_some_and(|first| first & 0xf0 == 0xc0)
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

/// What a path's kind says about forwarding through it: the relay's links
/// join every first flight sent through the relay.
impl socket_core::Forwarding for PathKind {
    fn relayed(&self) -> bool {
        matches!(self, PathKind::RelayUdp | PathKind::RelayTcp)
    }
}

/// What the socket saw that the path machines act on.
pub type PathEvent = socket_core::PathEvent<PathKind>;

#[derive(Default)]
struct State {
    /// Who each stand-in stands for and where each peer's packets go.
    peers: Peers<PathKind>,
    framed: HashMap<PathId, Arc<dyn FramedIo>>,
    inbound: VecDeque<(PathId, Vec<u8>)>,
    recv_waker: Option<Waker>,
    probe_answers: Option<mpsc::UnboundedSender<ProbeAnswered>>,
    path_events: Option<mpsc::UnboundedSender<PathEvent>>,
    forward_sink: Option<Arc<dyn ForwardSink>>,
}

impl State {
    fn tell(&mut self, event: PathEvent) {
        if let Some(events) = &self.path_events
            && events.send(event).is_err()
        {
            self.path_events = None;
            self.peers.report_arrivals(false);
        }
    }
}

/// A send that has nowhere to go, as quinn's socket reports it.
fn no_path(NoPath(reason): NoPath) -> io::Error {
    let kind = if reason == socket_core::NOT_A_STAND_IN {
        io::ErrorKind::AddrNotAvailable
    } else {
        io::ErrorKind::NotConnected
    };
    io::Error::new(kind, reason)
}

/// quinn's socket: one stand-in address per peer, one route per peer.
/// quinn sends to the stand-in; the layer puts the packet on the carrying
/// path, or on every candidate while the route sends on all, and maps
/// arrivals back to the stand-in whichever path brought them. Which peer
/// and which path are [`Peers`]' to say; this sends and receives.
pub struct LinkSocket {
    /// Swapped whole when the network under the profile changes.
    substrate: RwLock<Arc<dyn Datagrams>>,
    own: HostId,
    state: Mutex<State>,
    drops: DropCounts,
    /// What the peers' clock counts from.
    epoch: Instant,
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
            epoch: Instant::now(),
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
        Ok(Arc::new(Udp::new(bind_udp(bind, remembered_port)?, true)?))
    }

    /// The substrate over a UDP socket the caller already bound, which
    /// stays the caller's: a network change never binds it afresh.
    pub fn on_udp(socket: std::net::UdpSocket) -> io::Result<Arc<dyn Datagrams>> {
        Ok(Arc::new(Udp::new(socket, false)?))
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

    /// Binds the substrate afresh on its port, where it is the socket's own
    /// OS socket; stand-ins and routes stay.
    pub fn rebind_in_place(&self) -> io::Result<bool> {
        let rebound = self.substrate().rebind()?;
        if rebound && let Some(waker) = self.lock().recv_waker.take() {
            waker.wake();
        }
        Ok(rebound)
    }

    fn substrate(&self) -> Arc<dyn Datagrams> {
        self.substrate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn now_ms(&self) -> u64 {
        Instant::now()
            .saturating_duration_since(self.epoch)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// The address quinn dials and hears `peer` at; see
    /// [`Peers::stand_in`].
    pub fn stand_in(&self, peer: HostId) -> SocketAddr {
        let now = self.now_ms();
        self.lock().peers.stand_in(peer, now)
    }

    /// The host behind a stand-in, if it has said who it is.
    pub fn peer_of(&self, stand_in: SocketAddr) -> Option<HostId> {
        self.lock().peers.peer_of(stand_in)
    }

    /// The address quinn dials to reach whoever answers at `addr`; see
    /// [`Peers::stand_in_at`].
    pub fn stand_in_at(&self, addr: SocketAddr) -> SocketAddr {
        let now = self.now_ms();
        self.lock().peers.stand_in_at(plain(addr), now)
    }

    /// The address quinn dials to reach whoever answers on the byte-stream
    /// path `path`.
    pub fn stand_in_on(&self, path: PathId) -> SocketAddr {
        let now = self.now_ms();
        self.lock().peers.stand_in_on(path, now)
    }

    /// The network address behind a stand-in, where it is one.
    pub fn address_of(&self, stand_in: SocketAddr) -> Option<SocketAddr> {
        self.lock().peers.address_of(stand_in)
    }

    /// Gives an unnamed sender's stand-in its host once the handshake says
    /// who it is; see [`Peers::name`]. True when the stand-in is the host's.
    pub fn name(&self, stand_in: SocketAddr, host: HostId) -> bool {
        let now = self.now_ms();
        self.lock().peers.name(stand_in, host, now)
    }

    /// Holds `stand_in` as a dialled connection's until the hold drops;
    /// see [`Peers::dial_held`].
    pub fn hold_dial(self: &Arc<Self>, stand_in: SocketAddr) -> DialHold {
        self.lock().peers.dial_held(stand_in);
        DialHold {
            socket: Arc::downgrade(self),
            stand_in,
        }
    }

    /// `peer`'s last link went; see [`Peers::release`].
    pub fn release(&self, peer: HostId) {
        self.lock().peers.release(peer);
    }

    /// `peer`'s last link to this relay went; see [`Peers::let_go`].
    pub fn let_go(&self, peer: HostId) {
        self.lock().peers.let_go(peer);
    }

    /// `peer` holds a link to this relay again: it is kept until it is let
    /// go.
    pub fn keep(&self, peer: HostId) {
        self.lock().peers.keep(peer);
    }

    /// `peer` has a link again: its route is trusted once more.
    pub fn reclaim(&self, peer: HostId) {
        self.lock().peers.reclaim(peer);
    }

    /// Whether `peer`'s route is one a dial should trust; see
    /// [`Peers::trusted_route`].
    pub fn trusted_route(&self, peer: HostId) -> Option<Path> {
        self.lock().peers.trusted_route(peer)
    }

    /// Carries `peer`'s packets on one path.
    pub fn set_carrying(&self, peer: HostId, path: Path) {
        let now = self.now_ms();
        self.lock().peers.set_carrying(peer, path, now);
    }

    /// Sends none of `peer`'s packets until a path is named again; see
    /// [`Peers::stop_carrying`].
    pub fn stop_carrying(&self, peer: HostId) {
        self.lock().peers.stop_carrying(peer);
    }

    /// Sends each of `peer`'s packets on every one of `paths` until a
    /// packet from the peer arrives on one; see [`Peers::send_on_all`].
    pub fn send_on_all(&self, peer: HostId, paths: Vec<Path>, through_relay: bool) {
        let paths = paths.into_iter().map(plain_path).collect();
        let now = self.now_ms();
        self.lock()
            .peers
            .send_on_all(peer, paths, through_relay, now);
    }

    /// Dials `peer` on every one of `paths` at once, and through the relay
    /// when `through_relay`; see [`Peers::dial_on_all`]. Answers the
    /// stand-in quinn dials.
    pub fn dial_on_all(&self, peer: HostId, paths: Vec<Path>, through_relay: bool) -> SocketAddr {
        let paths = paths.into_iter().map(plain_path).collect();
        let now = self.now_ms();
        self.lock()
            .peers
            .dial_on_all(peer, paths, through_relay, now)
    }

    /// Dials `peer` through the forwarder at the far end of `through`; see
    /// [`Peers::dial_through`]. Answers the stand-in quinn dials.
    pub fn dial_through(&self, peer: HostId, through: LinkId) -> SocketAddr {
        let now = self.now_ms();
        self.lock().peers.dial_through(peer, through, now)
    }

    /// A link to `peer` came up on its connection at `stand_in`: whether
    /// that connection is what carries the peer here; see
    /// [`Peers::linked`].
    pub fn linked(&self, peer: HostId, stand_in: SocketAddr) -> bool {
        let now = self.now_ms();
        self.lock().peers.linked(peer, stand_in, now)
    }

    /// Registers a byte-stream path; its packets arrive through
    /// [`LinkSocket::deliver_framed`].
    pub fn add_framed(&self, io: Arc<dyn FramedIo>) -> PathId {
        let mut state = self.lock();
        let id = state.peers.add_framed();
        state.framed.insert(id, io);
        id
    }

    /// What a byte-stream path runs over, while it is registered.
    pub fn framed_kind(&self, path: PathId) -> Option<FramedKind> {
        Some(self.lock().framed.get(&path)?.kind())
    }

    /// Forgets a byte-stream path; see [`Peers::remove_framed`].
    pub fn remove_framed(&self, path: PathId) {
        let mut state = self.lock();
        state.framed.remove(&path);
        for event in state.peers.remove_framed(path) {
            state.tell(event);
        }
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
    /// this socket, and sends it the first flights it joins; see
    /// [`Peers::add_forwarding`].
    pub fn add_forwarding(&self, link: LinkId, via: SocketAddr, kind: PathKind) {
        let again = {
            let mut state = self.lock();
            let added = state.peers.add_forwarding(link, via, kind);
            for event in added.events {
                state.tell(event);
            }
            added.again
        };
        for again in again {
            self.send_again(again);
        }
    }

    /// Sends a first flight again on the path that now carries it.
    fn send_again(&self, again: Again) {
        let wire = self.lock().peers.wire(again.peer, again.path);
        for packet in &again.sent {
            if let Err(error) = wire
                .map_err(no_path)
                .and_then(|wire| self.put(wire, packet))
            {
                self.drops.sends.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(path = ?again.path, %error, "link socket dropped a first flight sent again");
            }
        }
    }

    /// Forgets a forwarding link; see [`Peers::remove_forwarding`].
    pub fn remove_forwarding(&self, link: LinkId) {
        let mut state = self.lock();
        if let Some(event) = state.peers.remove_forwarding(link) {
            state.tell(event);
        }
    }

    /// The forwarding link whose forwarder stands behind `stand_in`.
    pub fn forwarding_link(&self, stand_in: SocketAddr) -> Option<LinkId> {
        self.lock().peers.forwarding_link(stand_in)
    }

    /// What kind of path a link carried through `link` is: through the
    /// relay or through a paired daemon.
    pub fn forwarded_kind(&self, link: LinkId) -> Option<PathKind> {
        self.lock().peers.forwarded_kind(link)
    }

    /// Makes this the socket of a forwarder: a packet sent to it to
    /// forward goes to `sink`. Without one such packets are dropped.
    pub fn forward_to(&self, sink: Arc<dyn ForwardSink>) {
        self.lock().forward_sink = Some(sink);
    }

    /// The path a stand-in's packets go out on, where there is one.
    pub fn path_of(&self, stand_in: SocketAddr) -> Option<Path> {
        self.lock().peers.path_of(stand_in)
    }

    /// Every path a stand-in's packets go out on now; see
    /// [`socket_core::Peers::paths_of`].
    pub fn paths_of(&self, stand_in: SocketAddr) -> Vec<Path> {
        self.lock().peers.paths_of(stand_in)
    }

    /// What kind of path `path` is, as the link log names it, while the
    /// socket knows it.
    pub fn kind_of(&self, path: Path) -> Option<PathKind> {
        match path {
            Path::Direct(addr) => Some(PathKind::of_addr(addr)),
            Path::Punched(_) => Some(PathKind::Punched),
            Path::Forwarded(link) => self.forwarded_kind(link),
            Path::Framed(id) => Some(super::quic::stream_path(self.framed_kind(id)?)),
        }
    }

    /// Sends a datagram as it is, not through quinn, on the path carrying
    /// the peer behind `stand_in`: how a forwarder puts a packet it
    /// forwards on the destination's link.
    pub fn send_raw(&self, stand_in: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        let wire = self.lock().peers.raw(stand_in).map_err(no_path)?;
        self.put(wire, datagram)
    }

    /// Sends a probe to `peer` on `path`; its answer comes back on
    /// [`LinkSocket::probe_answers`].
    pub fn probe(&self, peer: HostId, path: Path, nonce: u64) -> io::Result<()> {
        let wire = self
            .lock()
            .peers
            .wire(Peer::Host(peer), path)
            .map_err(no_path)?;
        self.put(wire, &encode_probe(self.own, nonce))
    }

    /// The answers to this socket's probes from now on; a second call takes
    /// them from the first receiver.
    pub fn probe_answers(&self) -> mpsc::UnboundedReceiver<ProbeAnswered> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.lock().probe_answers = Some(sender);
        receiver
    }

    /// What the path machines act on from now on: forwarding links coming
    /// and going, and packets arriving. A second call takes them from the
    /// first receiver; the forwarding links up now come first.
    pub fn path_events(&self) -> mpsc::UnboundedReceiver<PathEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.lock();
        for (link, kind) in state.peers.forwarding_links() {
            let _ = sender.send(PathEvent::ForwardingUp { link, kind });
        }
        state.path_events = Some(sender);
        state.peers.report_arrivals(true);
        receiver
    }

    /// The path carrying `peer` now, when one path does.
    pub fn carrying(&self, peer: HostId) -> Option<Path> {
        self.lock().peers.carrying(peer)
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

    /// The largest datagram every path of this peer carries: the smallest
    /// of its paths', so a switch between them never needs a smaller one.
    pub fn max_datagram(&self, peer: HostId) -> usize {
        self.lock().peers.max_datagram(peer)
    }

    /// What the socket holds for its peers: their states, stand-ins,
    /// addresses and forwarding links.
    #[cfg(test)]
    pub(crate) fn held(&self) -> (usize, usize, usize, usize) {
        self.lock().peers.held()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Puts one datagram on the wire where [`Peers`] said it goes, behind
    /// the forwarding header when it goes to a forwarder.
    fn put(&self, wire: Wire, packet: &[u8]) -> io::Result<()> {
        let behind;
        let datagram = match wire.behind {
            Some(host) => {
                behind = encode_forwarded(host, packet);
                &behind[..]
            }
            None => packet,
        };
        match wire.to {
            WireTo::Udp(addr) => self.substrate().send_to(datagram, addr),
            WireTo::Framed(id) => {
                let io = self.lock().framed.get(&id).cloned().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "no such byte-stream path")
                })?;
                io.send(datagram)
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

    /// What a QUIC packet's arrival changed: the path machines hear of it,
    /// and a first flight it moved goes again.
    fn heard(&self, heard: Heard<PathKind>) -> SocketAddr {
        if let Some(again) = heard.again {
            self.send_again(again);
        }
        heard.stand_in
    }

    /// Handles one arrival in place: our own datagrams are answered or
    /// reported and dropped; a QUIC packet's stand-in and where it starts
    /// in `datagram` are returned.
    fn arrive(&self, arrival: Arrival, datagram: &[u8]) -> Option<(SocketAddr, usize)> {
        match parse(datagram).ok()? {
            PacketKind::Quic(_) => {
                let now = self.now_ms();
                let heard = {
                    let mut state = self.lock();
                    let heard = state.peers.arrived_quic(arrival, now);
                    if let Some(event) = heard.event {
                        state.tell(event);
                    }
                    heard
                };
                Some((self.heard(heard), 0))
            }
            PacketKind::Probe { from, nonce } => {
                self.lock().peers.probed_from(from, arrival);
                self.reply(arrival, &encode_probe_answer(self.own, nonce));
                None
            }
            PacketKind::ProbeAnswer { from, nonce } => {
                self.lock().peers.probed_from(from, arrival);
                self.answered(ProbeAnswered { from, nonce });
                None
            }
            PacketKind::Forwarded { host, payload } => {
                let now = self.now_ms();
                let (sink, sender) = {
                    let mut state = self.lock();
                    let sink = state.forward_sink.clone()?;
                    (sink, state.peers.arrived_to_forward(arrival, now))
                };
                sink.arrived(sender, host, payload);
                None
            }
            PacketKind::Delivered { host, payload } => match parse(payload).ok()? {
                PacketKind::Quic(_) => {
                    let now = self.now_ms();
                    let heard = {
                        let mut state = self.lock();
                        let heard =
                            state
                                .peers
                                .arrived_delivered(arrival, host, handshake(payload), now);
                        if let Some(event) = heard.event {
                            state.tell(event);
                        }
                        heard
                    };
                    Some((self.heard(heard), FORWARD_HEADER))
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
        let sends = self
            .lock()
            .peers
            .transmit(transmit.destination, transmit.contents);
        let put = |wire: Result<Wire, NoPath>| {
            wire.map_err(no_path)
                .and_then(|wire| self.put(wire, transmit.contents))
        };
        match sends {
            Sends::NotStandIn => {
                self.dropped_send(
                    transmit.destination,
                    &no_path(NoPath(socket_core::NOT_A_STAND_IN)),
                );
                Ok(())
            }
            Sends::Nowhere => Ok(()),
            Sends::One(wire) => match put(wire) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(error),
                Err(error) => {
                    self.dropped_send(transmit.destination, &error);
                    Ok(())
                }
                Ok(()) => Ok(()),
            },
            Sends::All(wires) => {
                // One path refusing is a lost copy; only every path being
                // full holds quinn back.
                let mut blocked = !wires.is_empty();
                for wire in wires {
                    match put(wire) {
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
pub(crate) fn plain(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(ip) => SocketAddr::new(ip.into(), v6.port()),
            None => addr,
        },
        SocketAddr::V4(_) => addr,
    }
}

/// A path in the address family the OS socket reports arrivals in.
fn plain_path(path: Path) -> Path {
    match path {
        Path::Direct(addr) => Path::Direct(plain(addr)),
        Path::Punched(addr) => Path::Punched(plain(addr)),
        path => path,
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
    /// None only while it is bound afresh.
    socket: RwLock<Option<tokio::net::UdpSocket>>,
    v6: bool,
    /// Whether the socket is this layer's to bind afresh, rather than one
    /// a caller bound and holds.
    rebindable: bool,
}

impl Udp {
    fn new(socket: std::net::UdpSocket, rebindable: bool) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let v6 = socket.local_addr()?.is_ipv6();
        Ok(Self {
            socket: RwLock::new(Some(tokio::net::UdpSocket::from_std(socket)?)),
            v6,
            rebindable,
        })
    }

    fn with<T>(
        &self,
        use_socket: impl FnOnce(&tokio::net::UdpSocket) -> io::Result<T>,
    ) -> io::Result<T> {
        match &*self
            .socket
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            Some(socket) => use_socket(socket),
            None => Err(io::ErrorKind::WouldBlock.into()),
        }
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
        self.with(|socket| socket.try_send_to(buf, to).map(|_| ()))
    }

    fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &*self
            .socket
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            Some(socket) => socket.poll_send_ready(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        let guard = self
            .socket
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(socket) = &*guard else {
            return Poll::Ready(Err(io::ErrorKind::WouldBlock.into()));
        };
        loop {
            let mut read = tokio::io::ReadBuf::new(buf);
            match socket.poll_recv_from(cx, &mut read) {
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
        self.with(tokio::net::UdpSocket::local_addr)
    }

    /// Closes the socket and binds its address again: the port peers know
    /// stays, or any port when another took it in between.
    fn rebind(&self) -> io::Result<bool> {
        if !self.rebindable {
            return Ok(false);
        }
        let mut held = self
            .socket
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(old) = held.take() else {
            return Ok(false);
        };
        let at = match old.local_addr() {
            Ok(at) => at,
            Err(error) => {
                *held = Some(old);
                return Err(error);
            }
        };
        drop(old);
        let fresh = bind_one(at.ip(), at.port()).or_else(|_| bind_one(at.ip(), 0))?;
        fresh.set_nonblocking(true)?;
        *held = Some(tokio::net::UdpSocket::from_std(fresh)?);
        Ok(true)
    }
}

/// A dialled connection's claim on its stand-in, for as long as the
/// connection is held.
pub struct DialHold {
    socket: std::sync::Weak<LinkSocket>,
    stand_in: SocketAddr,
}

impl Drop for DialHold {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.upgrade() {
            socket.lock().peers.dial_ended(self.stand_in);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use daemon_core::trust::PairingUpdate;

    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::trust::{SharedTrustStore, TrustStore};

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
            parse(&encode_probe(host, 7)[..socket_core::PROBE_LEN - 1]),
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

    /// A known host probing from another of its addresses is known there
    /// from then on: its link's packets from that address reach the same
    /// stand-in, so its path machine can move the link there.
    #[test]
    fn socket_knows_a_host_at_the_address_it_probes_from() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let (first, second): (SocketAddr, SocketAddr) = (
            "127.0.0.1:7000".parse().unwrap(),
            "192.168.1.9:7000".parse().unwrap(),
        );
        socket.set_carrying(peer, Path::Direct(first));
        let stand_in = socket.stand_in(peer);

        fake.push(second, &encode_probe(peer, 7));
        assert_eq!(
            receive(&socket),
            None,
            "a probe is answered, not handed to quinn"
        );
        fake.push(second, QUIC);
        assert_eq!(receive(&socket).map(|(at, _)| at), Some(stand_in));

        // A host this socket never heard of teaches it nothing.
        let stranger = HostId::new_v4();
        let third: SocketAddr = "192.168.1.10:7000".parse().unwrap();
        fake.push(third, &encode_probe(stranger, 8));
        assert_eq!(receive(&socket), None);
        fake.push(third, QUIC);
        assert_ne!(receive(&socket).map(|(at, _)| at), Some(stand_in));
        assert_eq!(socket.peer_of(socket.stand_in_at(third)), None);
    }

    /// A known host probing from an address that so far only sent this
    /// host packets it could not name, as a peer's dead connection to this
    /// host's last run does, is known there from then on.
    #[test]
    fn socket_knows_a_host_probing_from_where_strays_came_from() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let punched: SocketAddr = "192.168.1.9:7000".parse().unwrap();
        socket.set_carrying(peer, Path::Forwarded(LinkId(3)));
        let stand_in = socket.stand_in(peer);
        fake.push(punched, QUIC);
        let (stray, _) = receive(&socket).unwrap();
        assert_ne!(stray, stand_in);

        fake.push(punched, &encode_probe(peer, 7));
        assert_eq!(receive(&socket), None);
        fake.push(punched, QUIC);
        assert_eq!(receive(&socket).map(|(at, _)| at), Some(stand_in));
    }

    /// A known host probing from an address this host is still dialling
    /// unnamed leaves that address to the dial: quinn's client would drop
    /// the handshake's answers arriving at the host's stand-in instead.
    #[test]
    fn socket_leaves_an_address_a_dial_is_unnamed_at_to_the_dial() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let (known, dialled): (SocketAddr, SocketAddr) = (
            "127.0.0.1:7000".parse().unwrap(),
            "192.168.1.9:7000".parse().unwrap(),
        );
        socket.set_carrying(peer, Path::Direct(known));
        let dial = socket.stand_in_at(dialled);

        fake.push(dialled, &encode_probe(peer, 7));
        assert_eq!(receive(&socket), None);
        fake.push(dialled, QUIC);
        assert_eq!(
            receive(&socket).map(|(at, _)| at),
            Some(dial),
            "the answer reaches the dial"
        );
        assert!(socket.name(dial, peer), "and its handshake names it");
    }

    /// A first flight's route collapses to the path the peer answers on,
    /// even one the flight did not name: a host known at another address,
    /// or a flight that found no candidate at all.
    #[test]
    fn socket_carries_a_first_flight_where_the_peer_answers() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let (named, elsewhere): (SocketAddr, SocketAddr) = (
            "192.168.1.9:7000".parse().unwrap(),
            "192.168.1.10:7000".parse().unwrap(),
        );
        socket.set_carrying(peer, Path::Direct(elsewhere));
        socket.send_on_all(peer, vec![Path::Direct(named)], false);
        fake.push(elsewhere, QUIC);
        assert_eq!(
            receive(&socket).map(|(at, _)| at),
            Some(socket.stand_in(peer))
        );
        assert_eq!(socket.carrying(peer), Some(Path::Direct(elsewhere)));

        socket.send_on_all(peer, Vec::new(), false);
        fake.push(elsewhere, QUIC);
        receive(&socket);
        assert_eq!(socket.carrying(peer), Some(Path::Direct(elsewhere)));
        socket
            .try_send(&transmit(socket.stand_in(peer), QUIC))
            .unwrap();
        assert_eq!(fake.take_sent(), vec![(elsewhere, QUIC.to_vec())]);
    }

    /// The path a first flight settles on is only the first to answer:
    /// until a link comes up on it no dial trusts it, so the next goes out
    /// on every candidate again rather than only where it was turned away.
    #[test]
    fn socket_trusts_a_first_flights_path_only_once_a_link_comes_up_on_it() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let (refusing, real): (SocketAddr, SocketAddr) = (
            "192.168.1.9:7000".parse().unwrap(),
            "192.168.1.10:7000".parse().unwrap(),
        );
        let stand_in = socket.dial_on_all(
            peer,
            vec![Path::Direct(refusing), Path::Direct(real)],
            false,
        );
        fake.push(refusing, QUIC);
        receive(&socket);
        assert_eq!(socket.carrying(peer), Some(Path::Direct(refusing)));
        assert_eq!(socket.trusted_route(peer), None, "nothing linked on it");

        fake.take_sent();
        socket.dial_on_all(peer, vec![Path::Direct(real)], false);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(real, QUIC.to_vec())]);

        fake.push(real, QUIC);
        receive(&socket);
        socket.reclaim(peer);
        assert_eq!(socket.trusted_route(peer), Some(Path::Direct(real)));
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
        let config = format!(
            "{:?}",
            crate::identity::quic_transport_config(Default::default())
        );
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

    /// A host whose own dial crossed this host's: its connection, heard
    /// before this host dialled it, is at a stand-in of its own, and naming
    /// it there leaves the host at the stand-in this host's dial is at, so
    /// the dial still hears its answers; once that dial is gone, the host is
    /// named afresh where it is heard, as before.
    #[test]
    fn socket_keeps_a_dialled_hosts_stand_in_when_its_crossing_link_is_named() {
        let (fake, socket, _own) = fake_socket();
        let desk = HostId::new_v4();
        let at: SocketAddr = "192.168.1.9:7000".parse().unwrap();
        fake.push(at, QUIC);
        let (crossing, _) = receive(&socket).unwrap();
        let dialled = socket.dial_on_all(desk, vec![Path::Direct(at)], false);
        assert_ne!(crossing, dialled);
        let hold = socket.hold_dial(dialled);

        assert!(socket.name(crossing, desk));
        assert_eq!(socket.stand_in(desk), dialled, "the dial's stand-in stays");
        fake.push(at, QUIC);
        assert_eq!(receive(&socket).unwrap().0, dialled, "the dial hears it");
        socket.try_send(&transmit(crossing, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(at, QUIC.to_vec())],
            "the crossing link still reaches it"
        );

        drop(hold);
        let elsewhere: SocketAddr = "192.168.1.10:7000".parse().unwrap();
        fake.push(elsewhere, QUIC);
        let (moved, _) = receive(&socket).unwrap();
        assert!(socket.name(moved, desk));
        assert_eq!(socket.stand_in(desk), moved, "no dial holds it now");
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
    /// A burst of senders that never finish a handshake, and a forwarder
    /// delivering in the names of hosts no link follows, leave nothing
    /// behind once a minute passes without them: the socket holds what it
    /// held before. A sender that goes on sending without its handshake
    /// naming it is let go too, while a named host and a dialled forwarder
    /// still heard from stay.
    #[tokio::test(start_paused = true)]
    async fn socket_peer_count_returns_to_its_baseline_after_a_burst_of_idle_senders() {
        let (fake, socket, _own) = fake_socket();
        let lan: SocketAddr = "192.0.2.40:5000".parse().unwrap();
        let laptop = HostId::new_v4();
        fake.push(lan, QUIC);
        let (laptop_at, _) = receive(&socket).unwrap();
        assert!(socket.name(laptop_at, laptop));
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(1), via, PathKind::RelayUdp);
        let baseline = socket.held();

        let flood: IpAddr = "203.0.113.9".parse().unwrap();
        for port in 0..100 {
            fake.push(SocketAddr::new(flood, 1000 + port), QUIC);
            receive(&socket).unwrap();
        }
        for _ in 0..20 {
            fake.push(relay, &encode_delivered(HostId::new_v4(), QUIC));
            receive(&socket).unwrap();
        }
        let chatty: SocketAddr = "203.0.113.10:4000".parse().unwrap();
        fake.push(chatty, QUIC);
        let (chatty_at, _) = receive(&socket).unwrap();
        assert_eq!(socket.held().0, baseline.0 + 121);

        // The relay's link and the unnamed sender go on sending.
        for _ in 0..6 {
            tokio::time::advance(Duration::from_secs(10)).await;
            fake.push(relay, QUIC);
            assert_eq!(receive(&socket).unwrap().0, via);
            fake.push(chatty, QUIC);
            receive(&socket).unwrap();
        }
        assert_ne!(
            socket.stand_in_at(chatty),
            chatty_at,
            "a sender never named is let go, and minted afresh"
        );
        assert_eq!(socket.path_of(chatty_at), None);
        let (peers, stand_ins, addresses, forwarding) = socket.held();
        assert_eq!(
            (peers - 1, stand_ins - 1, addresses, forwarding),
            baseline,
            "only the chatty sender's fresh stand-in is new"
        );
        assert_eq!(socket.peer_of(laptop_at), Some(laptop));
        assert_eq!(socket.address_of(laptop_at), Some(lan));
        assert_eq!(socket.forwarding_link(via), Some(LinkId(1)));
    }

    /// Removing a byte-stream path forgets the sender on it that was never
    /// named, and removing a forwarding link forgets the hosts known only
    /// from its deliveries; a host this host dialled through it stays.
    #[test]
    fn socket_removing_a_path_forgets_the_senders_known_only_on_it() {
        let (fake, socket, _own) = fake_socket();
        let baseline = socket.held();
        let path = socket.add_framed(Arc::new(Recorded::default()));
        socket.deliver_framed(path, QUIC.to_vec());
        let (on_stream, _) = receive(&socket).unwrap();
        socket.remove_framed(path);
        assert_eq!(socket.path_of(on_stream), None);
        assert_eq!(socket.held(), baseline);

        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(1), via, PathKind::RelayUdp);
        let with_relay = socket.held();
        let dialled = HostId::new_v4();
        socket.set_carrying(dialled, Path::Forwarded(LinkId(1)));
        let dialled_at = socket.stand_in(dialled);
        let stranger = HostId::new_v4();
        fake.push(relay, &encode_delivered(stranger, QUIC));
        let (stranger_at, _) = receive(&socket).unwrap();
        assert_eq!(socket.peer_of(stranger_at), Some(stranger));
        socket.remove_forwarding(LinkId(1));
        assert_eq!(socket.peer_of(stranger_at), None);
        assert_eq!(socket.peer_of(dialled_at), Some(dialled));
        let (peers, stand_ins, addresses, _) = socket.held();
        assert_eq!(
            (peers, stand_ins, addresses),
            (with_relay.0 + 1, with_relay.1 + 1, with_relay.2)
        );
    }

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

    /// A host whose last link went keeps its route for what its closing
    /// connection still sends, but no dial trusts it, and a connection
    /// from it elsewhere names it there; its next link takes that back.
    #[test]
    fn socket_releases_a_host_whose_last_link_went() {
        let (fake, socket, _own) = fake_socket();
        let (old, new): (SocketAddr, SocketAddr) = (
            "192.0.2.50:5000".parse().unwrap(),
            "192.0.2.51:5000".parse().unwrap(),
        );
        let desk = HostId::new_v4();
        let dialled = socket.stand_in_at(old);
        assert!(socket.name(dialled, desk));
        assert_eq!(socket.trusted_route(desk), Some(Path::Direct(old)));
        socket.release(desk);
        assert_eq!(socket.trusted_route(desk), None, "no dial trusts it");
        socket.try_send(&transmit(dialled, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(old, QUIC.to_vec())]);
        socket.reclaim(desk);
        assert_eq!(socket.trusted_route(desk), Some(Path::Direct(old)));

        socket.release(desk);
        socket.set_carrying(desk, Path::Forwarded(LinkId(5)));
        socket.release(desk);
        fake.push(new, QUIC);
        let (moved, _) = receive(&socket).unwrap();
        assert!(socket.name(moved, desk), "named afresh where it came from");
        assert_eq!(socket.trusted_route(desk), Some(Path::Direct(new)));
    }

    /// A link to the relay over TCP whose stream ends before QUIC closes
    /// the link is reported gone once, as the stream goes, so the path
    /// machines stop holding a relay path that can never answer.
    #[test]
    fn a_relay_link_over_a_stream_that_ends_is_reported_gone_once() {
        let (_fake, socket, _own) = fake_socket();
        let stream = Arc::new(Recorded::default());
        let path = socket.add_framed(stream);
        let via = socket.stand_in_on(path);
        socket.add_forwarding(LinkId(7), via, PathKind::RelayTcp);
        let mut events = socket.path_events();
        assert!(matches!(
            events.try_recv(),
            Ok(PathEvent::ForwardingUp {
                link: LinkId(7),
                ..
            })
        ));

        socket.remove_framed(path);
        assert!(matches!(
            events.try_recv(),
            Ok(PathEvent::ForwardingDown { link: LinkId(7) })
        ));

        socket.remove_forwarding(LinkId(7));
        assert!(events.try_recv().is_err());
    }

    /// A forwarder silent past the time unnamed senders are kept is kept
    /// while its link is up, and so are the routes through it: a phone
    /// suspended for minutes hears again before QUIC closes the link.
    #[tokio::test(start_paused = true)]
    async fn socket_keeps_a_silent_forwarder_while_its_link_is_up() {
        let (fake, socket, _own) = fake_socket();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(5), via, PathKind::RelayUdp);
        let desk = HostId::new_v4();
        socket.set_carrying(desk, Path::Forwarded(LinkId(5)));
        let stand_in = socket.stand_in(desk);

        tokio::time::advance(std::time::Duration::from_millis(
            socket_core::UNCLAIMED_PEER_TIMEOUT_MS * 2,
        ))
        .await;
        fake.push("192.0.2.61:5000".parse().unwrap(), QUIC);
        receive(&socket).unwrap();

        assert_eq!(socket.forwarding_link(via), Some(LinkId(5)));
        assert_eq!(socket.path_of(stand_in), Some(Path::Forwarded(LinkId(5))));
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(relay, encode_forwarded(desk, QUIC))]
        );
    }

    /// A sender whose handshake is done but that the socket cannot name,
    /// because its host is carried another way, is kept while it sends:
    /// a crossed direct link next to a relayed one stays where it is.
    #[tokio::test(start_paused = true)]
    async fn socket_keeps_a_sender_it_cannot_name_while_it_sends() {
        let (fake, socket, _own) = fake_socket();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(5), via, PathKind::RelayUdp);
        let desk = HostId::new_v4();
        socket.set_carrying(desk, Path::Forwarded(LinkId(5)));
        let lan: SocketAddr = "192.0.2.60:5000".parse().unwrap();
        fake.push(lan, QUIC);
        let (direct, _) = receive(&socket).unwrap();
        assert!(!socket.name(direct, desk));

        for _ in 0..12 {
            tokio::time::advance(Duration::from_secs(10)).await;
            fake.push(lan, QUIC);
            assert_eq!(receive(&socket).unwrap().0, direct);
        }
        assert_eq!(socket.stand_in_at(lan), direct);
        assert_eq!(socket.address_of(direct), Some(lan));
        socket.try_send(&transmit(direct, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(lan, QUIC.to_vec())]);

        // Once it stops, it is let go like any silent sender.
        tokio::time::advance(std::time::Duration::from_millis(
            socket_core::UNCLAIMED_PEER_TIMEOUT_MS,
        ))
        .await;
        fake.push("192.0.2.61:5000".parse().unwrap(), QUIC);
        receive(&socket).unwrap();
        assert_eq!(socket.path_of(direct), None);
    }

    /// A host whose byte-stream link ended is named where it comes back
    /// over UDP.
    #[test]
    fn socket_names_a_host_back_over_udp_once_its_byte_stream_goes() {
        let (fake, socket, _own) = fake_socket();
        let stream = Arc::new(Recorded::default());
        let path = socket.add_framed(stream.clone());
        socket.deliver_framed(path, QUIC.to_vec());
        let (on_stream, _) = receive(&socket).unwrap();
        let host = HostId::new_v4();
        assert!(socket.name(on_stream, host));
        assert_eq!(socket.path_of(on_stream), Some(Path::Framed(path)));

        socket.remove_framed(path);
        assert_eq!(socket.path_of(on_stream), None);
        let back: SocketAddr = "203.0.113.7:6100".parse().unwrap();
        fake.push(back, QUIC);
        let (over_udp, _) = receive(&socket).unwrap();
        assert!(socket.name(over_udp, host));
        assert_eq!(socket.stand_in(host), over_udp);
        assert_eq!(socket.address_of(over_udp), Some(back));
        socket.try_send(&transmit(on_stream, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(back, QUIC.to_vec())]);
        fake.push(back, QUIC);
        assert_eq!(receive(&socket).unwrap().0, over_udp);
    }

    /// Senders from one IP stand in at one IP, each on a port of its own,
    /// so quinn's address validation, which names only the IP, carries
    /// over to a sender's new port; senders from other IPs, hosts and byte
    /// streams stand in elsewhere.
    #[test]
    fn socket_senders_from_one_ip_share_a_stand_in_ip() {
        let (fake, socket, _own) = fake_socket();
        let mut heard = Vec::new();
        for from in [
            "192.0.2.70:5000",
            "192.0.2.70:5001",
            "[::ffff:192.0.2.70]:5002",
            "192.0.2.71:5000",
            "[2001:db8::7]:5000",
            "[2001:db8::7]:5001",
        ] {
            fake.push(from.parse().unwrap(), QUIC);
            heard.push(receive(&socket).unwrap().0);
        }
        let host = socket.stand_in(HostId::new_v4());
        let path = socket.add_framed(Arc::new(Recorded::default()));
        let on_stream = socket.stand_in_on(path);
        let ip = |at: SocketAddr| at.ip();
        assert_eq!(ip(heard[0]), ip(heard[1]));
        assert_eq!(ip(heard[0]), ip(heard[2]));
        assert_ne!(ip(heard[0]), ip(heard[3]));
        assert_eq!(ip(heard[4]), ip(heard[5]));
        assert_ne!(ip(heard[0]), ip(heard[4]));
        let mut all = heard.clone();
        all.extend([host, on_stream]);
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "every stand-in is its own");
        for stand_in in all {
            let IpAddr::V6(v6) = stand_in.ip() else {
                panic!("{stand_in} is not in the stand-in prefix");
            };
            assert_eq!(v6.segments()[..3], socket_core::STAND_IN_PREFIX);
        }
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
            false,
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

        // One full path is a lost copy; every path full holds quinn back.
        fake.blocked.lock().unwrap().push(lan);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent().len(), 2);
        fake.blocked.lock().unwrap().extend([public, relay]);
        let error = socket.try_send(&transmit(stand_in, QUIC)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        fake.blocked.lock().unwrap().clear();

        // The first answer reaches the one stand-in, and the path it came
        // on carries the link from then on; answers on the others still
        // reach the stand-in.
        fake.push(public, QUIC);
        assert_eq!(receive(&socket).unwrap().0, stand_in);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(public, QUIC.to_vec())]);
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket).unwrap().0, stand_in);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(public, QUIC.to_vec())]);

        // A path chosen by hand carries it the same way.
        socket.set_carrying(peer, Path::Direct(lan));
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(lan, QUIC.to_vec())]);
    }

    /// A relay dial moves the forwarding link from the attempt that said
    /// its Hello first to the one it keeps: the first flight goes again
    /// to the new address, since what went to the old may have gone
    /// nowhere. The same address again sends nothing twice.
    #[test]
    fn socket_sends_a_first_flight_again_where_its_forwarding_moves() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let (first, kept): (SocketAddr, SocketAddr) = (
            "[2001:db8::1]:9001".parse().unwrap(),
            "198.51.100.1:9001".parse().unwrap(),
        );
        let stand_in = socket.stand_in(peer);
        socket.send_on_all(peer, Vec::new(), true);
        socket.add_forwarding(LinkId(7), socket.stand_in_at(first), PathKind::RelayUdp);
        socket.try_send(&transmit(stand_in, b"\x40first")).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(first, encode_forwarded(peer, b"\x40first"))]
        );

        socket.add_forwarding(LinkId(7), socket.stand_in_at(kept), PathKind::RelayUdp);
        assert_eq!(
            fake.take_sent(),
            vec![(kept, encode_forwarded(peer, b"\x40first"))]
        );
        socket.add_forwarding(LinkId(7), socket.stand_in_at(kept), PathKind::RelayUdp);
        assert_eq!(fake.take_sent(), Vec::new());
    }

    /// A host started again dials its peer only through the relay, at an
    /// address nothing answers at, while the peer's link from the last run
    /// still probes the address this run took over. Once that link's
    /// packets carry the dial, the flight goes again where they came from,
    /// and only there: what went out so far would otherwise wait out a
    /// retransmit.
    #[test]
    fn socket_sends_a_first_flight_again_where_an_arrival_moves_it() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let (relay, punched): (SocketAddr, SocketAddr) = (
            "[2001:db8::1]:9001".parse().unwrap(),
            "192.168.1.9:7000".parse().unwrap(),
        );
        let stand_in = socket.stand_in(peer);
        socket.send_on_all(peer, Vec::new(), true);
        socket.add_forwarding(LinkId(7), socket.stand_in_at(relay), PathKind::RelayUdp);
        socket.try_send(&transmit(stand_in, b"\x40first")).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(relay, encode_forwarded(peer, b"\x40first"))]
        );

        fake.push(punched, &encode_probe(peer, 1));
        assert!(receive(&socket).is_none());
        assert_eq!(
            fake.take_sent(),
            vec![(punched, encode_probe_answer(socket.own, 1).to_vec())]
        );
        fake.push(punched, QUIC);
        assert_eq!(receive(&socket).unwrap().0, stand_in);
        assert_eq!(fake.take_sent(), vec![(punched, b"\x40first".to_vec())]);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(fake.take_sent(), vec![(punched, QUIC.to_vec())]);
    }

    /// An arrival on a path the flight already took sends nothing twice.
    #[test]
    fn socket_sends_no_first_flight_again_where_it_already_went() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let stand_in = socket.stand_in(peer);
        socket.send_on_all(peer, vec![Path::Direct(lan)], true);
        socket.try_send(&transmit(stand_in, b"\x40first")).unwrap();
        assert_eq!(fake.take_sent(), vec![(lan, b"\x40first".to_vec())]);
        fake.push(lan, QUIC);
        assert_eq!(receive(&socket).unwrap().0, stand_in);
        assert_eq!(fake.take_sent(), Vec::new());
    }

    #[test]
    fn socket_sends_a_first_flight_through_a_relay_that_joins_late() {
        let (fake, socket, _own) = fake_socket();
        let peer = HostId::new_v4();
        let lan: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let relay: SocketAddr = "198.51.100.1:443".parse().unwrap();
        let stand_in = socket.stand_in(peer);
        socket.send_on_all(peer, vec![Path::Direct(lan)], true);
        socket.try_send(&transmit(stand_in, b"\x40first")).unwrap();
        socket.try_send(&transmit(stand_in, b"\x40second")).unwrap();
        assert_eq!(fake.take_sent().len(), 2);

        // The relay's connection carries data before the relay admits it:
        // the first flight goes through it at once, what went out before
        // first.
        let via = socket.stand_in_at(relay);
        socket.add_forwarding(LinkId(7), via, PathKind::RelayUdp);
        assert_eq!(
            fake.take_sent(),
            vec![
                (relay, encode_forwarded(peer, b"\x40first")),
                (relay, encode_forwarded(peer, b"\x40second")),
            ]
        );
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(lan, QUIC.to_vec()), (relay, encode_forwarded(peer, QUIC))]
        );

        // Admitted, the relay's link is named afresh; the route follows.
        socket.add_forwarding(LinkId(8), via, PathKind::RelayUdp);
        assert_eq!(socket.forwarded_kind(LinkId(7)), None);
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(lan, QUIC.to_vec()), (relay, encode_forwarded(peer, QUIC))]
        );

        // The host answers through the relay first: the relay carries it.
        fake.push(relay, &encode_delivered(peer, QUIC));
        assert_eq!(receive(&socket).unwrap().0, stand_in);
        assert_eq!(socket.path_of(stand_in), Some(Path::Forwarded(LinkId(8))));
        socket.try_send(&transmit(stand_in, QUIC)).unwrap();
        assert_eq!(
            fake.take_sent(),
            vec![(relay, encode_forwarded(peer, QUIC))]
        );

        // A flight that does not go through the relay is not joined.
        let other = HostId::new_v4();
        socket.send_on_all(other, vec![Path::Direct(lan)], false);
        socket.add_forwarding(
            LinkId(9),
            socket.stand_in_at("198.51.100.2:443".parse().unwrap()),
            PathKind::RelayUdp,
        );
        assert!(fake.take_sent().is_empty());
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

    /// Taking the remembered port again is best effort: anything on the
    /// machine may have it by now. Only the fallback is certain here, the
    /// port held by this test's own socket.
    #[tokio::test]
    async fn socket_bind_takes_another_port_when_the_remembered_one_is_held() {
        let holder = LinkSocket::bind_dual_stack("0.0.0.0:0".parse().unwrap(), None).unwrap();
        let port = holder.local_addr().unwrap().port();
        // Taken: any port rather than none.
        let other = LinkSocket::bind_dual_stack("0.0.0.0:0".parse().unwrap(), Some(port)).unwrap();
        assert_ne!(other.local_addr().unwrap().port(), port);
        // An explicit address is bound as given.
        let loopback =
            LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), Some(port)).unwrap();
        assert_eq!(loopback.local_addr().unwrap().ip(), Ipv4Addr::LOCALHOST);
        drop(holder);
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
            PairingUpdate::Inserted
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
                    .quic_server_config(
                        trust_for(&client_identity),
                        &crate::link::resumption::SessionTickets::in_memory(16),
                    )
                    .unwrap(),
            ),
        );
        let client_endpoint = endpoint(client_socket.clone(), None);

        let dead = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        client_socket.send_on_all(
            server_identity.host_id,
            vec![Path::Direct(dead_addr), Path::Direct(server_addr)],
            false,
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
            .quic_server_config(
                trust_for(&client_identity),
                &crate::link::resumption::SessionTickets::in_memory(16),
            )
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
        let link = loopback(crate::identity::quic_transport_config(Default::default())).await;
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
        let link = loopback(crate::identity::quic_transport_config(Default::default())).await;
        link.server_net.recv_errors.store(5, Ordering::SeqCst);
        assert_eq!(round_trip(&link, b"after errors").await, b"after errors");
        assert_eq!(link.server_socket.drops().receive_errors, 5);
        assert_eq!(link.server.close_reason(), None);
    }
}
