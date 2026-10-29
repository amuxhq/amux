//! The link socket: which peer each arrival is from and where each peer's
//! packets go, explored over the real `socket_core::Peers` of two hosts.
//!
//! The world is a laptop and a desk, paired, each signed in to the relay,
//! with a link between them on the desk's address on the local network. The
//! relay's name lists an address nothing answers at before the one it
//! listens on, so a dial to it may say its Hello where no one hears first.
//! QUIC is cut down to what the socket sees of it: a connection is a dial's
//! first flight (one Initial), the answer that brings it up, and the odd
//! packet either end sends once it is up; quinn takes a packet for a
//! connection only from the address it dialled or accepted it at.
//!
//! The world may do this, each within its bound:
//!
//! - the laptop's process is killed and started again, on the port it had or
//!   on another; the desk's end of the dead run's link stays up, sending,
//!   until it times out;
//! - the laptop dials its relay link, at the silent address or the one that
//!   answers, and the relay admits it; the link goes and is dialled again;
//! - the laptop dials the desk on every path it knows, or through the relay;
//! - either end of a connection sends a packet; either host's path machine
//!   probes the address it knows the other at, and moves the link there once
//!   the probe is answered;
//! - the local network between them goes; a link that can no longer reach
//!   its peer times out at either end;
//! - any datagram in flight is delivered (in any order), lost or delivered
//!   twice; the clock moves past the time unnamed senders are kept.
//!
//! A first flight lost to the fault is sent again as QUIC would, once; one
//! that went where nothing answers is not, so a dial the socket stranded
//! never links, and the liveness property says so.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use socket_core::{
    Arrival, Forwarding, Heard, HostId, LinkId, NoPath, PacketKind, Path, Peer, Peers, Transmit,
    Wire, WireTo, encode_delivered, encode_forwarded, encode_probe, encode_probe_answer, parse,
};

use crate::{Fault, Host, Machine, MsgIndex, Property};

const LAPTOP_ID: HostId = HostId::from_u128(0x1a97_0000_0000_4000_8000_0000_0000_0001);
const DESK_ID: HostId = HostId::from_u128(0xde5c_0000_0000_4000_8000_0000_0000_0002);

const LAPTOP_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 7000);
const LAPTOP_MOVED: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 7001);
const DESK_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30)), 7000);
const RELAY_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 443);
/// The first address of the relay's name, where nothing listens.
const RELAY_SILENT: SocketAddr = SocketAddr::new(
    IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
    443,
);

/// The desk's link to the relay, up throughout.
const DESK_RELAY: LinkId = LinkId(1);
/// Past this the socket lets an unnamed sender go.
const PAST_UNCLAIMED_MS: u64 = socket_core::UNCLAIMED_PEER_TIMEOUT_MS + 1;

/// What a forwarding link is here: every one reaches the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Relayed;

impl Forwarding for Relayed {
    fn relayed(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Who {
    Laptop,
    Desk,
}

impl Who {
    fn id(self) -> HostId {
        match self {
            Who::Laptop => LAPTOP_ID,
            Who::Desk => DESK_ID,
        }
    }

    fn other(self) -> Who {
        match self {
            Who::Laptop => Who::Desk,
            Who::Desk => Who::Laptop,
        }
    }
}

/// What a cut-down QUIC packet is. Its bytes are what quinn would hand the
/// socket: the first byte as QUIC's (an Initial's long header, or a short
/// one), then the connection and the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Quic {
    /// A dial's first flight.
    Initial,
    /// The answer that brings the connection up at both ends.
    Answer,
    /// Anything an end sends once its connection is up.
    Data,
}

fn quic(conn: u8, kind: Quic) -> Vec<u8> {
    let first = match kind {
        Quic::Initial => 0xc0,
        Quic::Answer | Quic::Data => 0x40,
    };
    let kind = match kind {
        Quic::Initial => 0,
        Quic::Answer => 1,
        Quic::Data => 2,
    };
    vec![first, conn, kind]
}

fn read_quic(bytes: &[u8]) -> Option<(u8, Quic)> {
    let kind = match bytes.get(2)? {
        0 => Quic::Initial,
        1 => Quic::Answer,
        2 => Quic::Data,
        _ => return None,
    };
    Some((*bytes.get(1)?, kind))
}

/// Whether a QUIC packet is a handshake's, as the node reads its header.
fn handshake(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(|first| first & 0xf0 == 0xc0)
}

/// One datagram on the wire, with the process that truly sent what it
/// carries: the properties judge the socket's attributions against it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Datagram {
    from: SocketAddr,
    to: SocketAddr,
    bytes: Vec<u8>,
}

/// One host's process.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Side {
    peers: Peers<Relayed>,
    /// Which run of the process this is.
    life: u8,
    addr: SocketAddr,
    /// The process's link to the relay, once dialled.
    relay: Option<RelayLink>,
    /// The path machine's round of probes: a link coming up starts one,
    /// since what answered for the last link says nothing of this one. Its
    /// probes carry it as their nonce, and an answer from another round
    /// is not taken for this one's, as the driver passes the machine only
    /// the outstanding probe's.
    round: u8,
    /// The paths to the other host a probe of this round was answered on.
    answered: BTreeSet<Path>,
    /// Probes the path machine sent this round.
    probes: u8,
    /// Dials this run.
    dials: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RelayLink {
    link: LinkId,
    /// Said its Hello at the silent address first.
    tried_silent: bool,
    /// The relay admitted it.
    admitted: bool,
}

impl Side {
    fn new(life: u8, addr: SocketAddr) -> Self {
        let mut peers = Peers::default();
        peers.report_arrivals(true);
        Self {
            peers,
            life,
            addr,
            relay: None,
            round: 0,
            answered: BTreeSet::new(),
            probes: 0,
            dials: 0,
        }
    }

    /// Where the process knows the other host's own address.
    fn knows_at(&self, who: Who) -> SocketAddr {
        match who {
            // The laptop knows the desk at its local address.
            Who::Laptop => DESK_ADDR,
            // The desk knows the laptop where its first run was.
            Who::Desk => LAPTOP_ADDR,
        }
    }
}

/// The relay: where each signed-in host's link to it is, and what it holds
/// from a connection it has yet to admit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Relay {
    at: BTreeMap<Who, SocketAddr>,
    held: Vec<(SocketAddr, HostId, Vec<u8>)>,
}

/// One connection, at both ends.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Conn {
    id: u8,
    client: Who,
    client_life: u8,
    server: Who,
    server_life: u8,
    /// The stand-in the client dialled.
    client_remote: SocketAddr,
    /// The stand-in the server accepted it at.
    server_remote: Option<SocketAddr>,
    client_up: bool,
    server_up: bool,
    client_dead: bool,
    server_dead: bool,
    /// Where the client's first flight went.
    sent_on: BTreeSet<Wire>,
    /// Of its packets, how many the fault lost, and how many times the
    /// client sent its first flight again for it.
    lost: u8,
    again: u8,
    /// Packets each end sent once up.
    sent: u8,
    /// It was up when the local network went: it can only time out.
    stranded: bool,
}

impl Conn {
    fn alive(&self, who: Who, life: u8) -> bool {
        if who == self.client {
            self.client_life == life && !self.client_dead
        } else {
            self.server_life == life && !self.server_dead
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct State {
    laptop: Side,
    desk: Side,
    relay: Relay,
    conns: Vec<Conn>,
    /// Kept sorted: a delivery may pick any of them.
    in_flight: Vec<Datagram>,
    now_ms: u64,
    restarts: u8,
    losses: u8,
    duplicates: u8,
    ticks: u8,
    relay_downs: u8,
    lan_down: bool,
    /// Dials that gave their handshake up.
    gave_up: u8,
    /// What the socket did that a property forbids, as it happened.
    discarded: Option<String>,
    /// A first flight went again on a path an arrival or a forwarding link
    /// moved it to.
    sent_again: bool,
}

impl State {
    fn side(&self, who: Who) -> &Side {
        match who {
            Who::Laptop => &self.laptop,
            Who::Desk => &self.desk,
        }
    }

    fn side_mut(&mut self, who: Who) -> &mut Side {
        match who {
            Who::Laptop => &mut self.laptop,
            Who::Desk => &mut self.desk,
        }
    }

    fn send(&mut self, datagram: Datagram) {
        let at = self.in_flight.partition_point(|held| *held <= datagram);
        self.in_flight.insert(at, datagram);
    }

    /// Whether the network carries a datagram from `from` to `to`.
    fn carries(&self, from: SocketAddr, to: SocketAddr) -> bool {
        let laptop = |addr: SocketAddr| addr == LAPTOP_ADDR || addr == LAPTOP_MOVED;
        let lan = (laptop(from) && to == DESK_ADDR) || (from == DESK_ADDR && laptop(to));
        !(lan && self.lan_down)
    }

    /// Puts `bytes` on the wire from `who` where the socket said.
    fn put(&mut self, who: Who, wire: Wire, bytes: &[u8]) {
        let WireTo::Udp(to) = wire.to else {
            unreachable!("no byte streams here");
        };
        let bytes = match wire.behind {
            Some(host) => encode_forwarded(host, bytes),
            None => bytes.to_vec(),
        };
        let from = self.side(who).addr;
        self.send(Datagram { from, to, bytes });
    }

    /// Sends what one of quinn's datagrams became, recording where a
    /// client's first flight went.
    fn transmit(&mut self, who: Who, stand_in: SocketAddr, bytes: &[u8], flight: Option<u8>) {
        let wires: Vec<Wire> = match self.side_mut(who).peers.transmit(stand_in, bytes) {
            Transmit::NotStandIn | Transmit::Nowhere => Vec::new(),
            Transmit::One(wire) => wire.into_iter().collect(),
            Transmit::All(wires) => wires.into_iter().filter_map(Result::ok).collect(),
        };
        for wire in wires {
            if let Some(conn) = flight
                && let Some(conn) = self.conns.iter_mut().find(|held| held.id == conn)
            {
                conn.sent_on.insert(wire);
            }
            self.put(who, wire, bytes);
        }
    }

    /// What an arrival changed: a first flight it moved goes again.
    fn heard(&mut self, who: Who, heard: Heard<Relayed>) -> SocketAddr {
        if let Some(again) = heard.again {
            self.sent_again = true;
            let wire = self.side(who).peers.wire(again.peer, again.path);
            if let Ok(wire) = wire {
                for bytes in &again.sent {
                    self.record_flight(who, bytes, wire);
                    self.put(who, wire, bytes);
                }
            }
        }
        heard.stand_in
    }

    /// A first flight's packet that went again on `wire`.
    fn record_flight(&mut self, who: Who, bytes: &[u8], wire: Wire) {
        let life = self.side(who).life;
        if let Some((id, Quic::Initial)) = read_quic(bytes)
            && let Some(conn) = self
                .conns
                .iter_mut()
                .find(|conn| conn.id == id && conn.client == who && conn.client_life == life)
        {
            conn.sent_on.insert(wire);
        }
    }

    /// A datagram reached the relay.
    fn at_relay(&mut self, from: SocketAddr, bytes: &[u8]) {
        let Ok(PacketKind::Forwarded { host: dst, payload }) = parse(bytes) else {
            return;
        };
        let sender = self
            .relay
            .at
            .iter()
            .find(|(_, at)| **at == from)
            .map(|(who, _)| *who);
        match sender {
            Some(sender) => self.forward(sender, dst, payload),
            None => self.relay.held.push((from, dst, payload.to_vec())),
        }
    }

    fn forward(&mut self, sender: Who, dst: HostId, payload: &[u8]) {
        let to = [Who::Laptop, Who::Desk]
            .into_iter()
            .find(|who| who.id() == dst)
            .and_then(|who| self.relay.at.get(&who).copied());
        match to {
            Some(to) => self.send(Datagram {
                from: RELAY_ADDR,
                to,
                bytes: encode_delivered(sender.id(), payload),
            }),
            // The relay has no link to the host: the packet is lost, as
            // QUIC sees it.
            None => self.lost(payload),
        }
    }

    /// A connection's packet was lost on the way: QUIC may send its first
    /// flight again, or give the handshake up and dial afresh.
    fn lost(&mut self, packet: &[u8]) {
        if let Some((id, _)) = read_quic(packet)
            && let Some(conn) = self.conns.iter_mut().find(|conn| conn.id == id)
        {
            conn.lost += 1;
        }
    }

    /// The relay admits `who`'s link from its address, and lets go of what
    /// it held from there.
    fn admit(&mut self, who: Who) {
        let at = self.side(who).addr;
        self.relay.at.insert(who, at);
        let held: Vec<_> = self
            .relay
            .held
            .iter()
            .filter(|(from, _, _)| *from == at)
            .cloned()
            .collect();
        self.relay.held.retain(|(from, _, _)| *from != at);
        for (_, dst, payload) in held {
            self.forward(who, dst, &payload);
        }
    }

    /// A datagram reached `who`'s process.
    fn at_host(&mut self, who: Who, from: SocketAddr, bytes: &[u8]) {
        let now = self.now_ms;
        let arrival = Arrival::Udp(from);
        match parse(bytes) {
            Ok(PacketKind::Quic(packet)) => {
                let heard = self.side_mut(who).peers.arrived_quic(arrival, now);
                let stand_in = self.heard(who, heard);
                self.endpoint(who, stand_in, packet);
            }
            Ok(PacketKind::Probe { from: host, nonce }) => {
                self.side_mut(who).peers.probed_from(host, arrival);
                let own = self.side(who).addr;
                self.send(Datagram {
                    from: own,
                    to: from,
                    bytes: encode_probe_answer(who.id(), nonce).to_vec(),
                });
            }
            Ok(PacketKind::ProbeAnswer { from: host, nonce }) => {
                let side = self.side_mut(who);
                side.peers.probed_from(host, arrival);
                if nonce == u64::from(side.round) {
                    side.answered.insert(Path::Direct(from));
                }
            }
            Ok(PacketKind::Delivered { host, payload }) => {
                if let Ok(PacketKind::Quic(packet)) = parse(payload) {
                    let heard = self.side_mut(who).peers.arrived_delivered(
                        arrival,
                        host,
                        handshake(packet),
                        now,
                    );
                    let stand_in = self.heard(who, heard);
                    self.endpoint(who, stand_in, packet);
                }
            }
            Ok(PacketKind::Forwarded { .. }) | Err(_) => {}
        }
    }

    /// quinn at `who` takes a QUIC packet from `stand_in`.
    fn endpoint(&mut self, who: Who, stand_in: SocketAddr, packet: &[u8]) {
        let Some((id, kind)) = read_quic(packet) else {
            return;
        };
        let life = self.side(who).life;
        let Some(at) = self.conns.iter().position(|conn| conn.id == id) else {
            return;
        };
        let conn = self.conns[at].clone();
        // A packet for a connection this process does not hold (its last
        // run's) is dropped, as quinn drops one it cannot place.
        if !conn.alive(who, life) {
            return;
        }
        if who == conn.server {
            match (kind, conn.server_remote) {
                (Quic::Initial, None) => {
                    let conn = &mut self.conns[at];
                    conn.server_remote = Some(stand_in);
                    conn.server_up = true;
                    let peer = conn.client.id();
                    self.link_up(who, peer, stand_in);
                    self.transmit(who, stand_in, &quic(id, Quic::Answer), None);
                }
                (Quic::Initial, Some(accepted)) if accepted == stand_in => {
                    self.transmit(who, stand_in, &quic(id, Quic::Answer), None);
                }
                // A copy of the first flight on another path, or a packet
                // from an address quinn has not validated: dropped.
                _ => {}
            }
            return;
        }
        // The client takes a packet only from the address it dialled.
        if stand_in != conn.client_remote {
            if self.discarded.is_none() {
                self.discarded = Some(format!(
                    "the {who:?} dropped connection {id}'s {kind:?} from {stand_in}, dialled at {}",
                    conn.client_remote
                ));
            }
            return;
        }
        if kind == Quic::Answer && !conn.client_up {
            self.conns[at].client_up = true;
            let peer = conn.server.id();
            let remote = conn.client_remote;
            self.link_up(who, peer, remote);
        }
    }

    /// A link to `peer` came up at `who` on its connection at `stand_in`;
    /// the path machine probes afresh.
    fn link_up(&mut self, who: Who, peer: HostId, stand_in: SocketAddr) {
        let now = self.now_ms;
        let side = self.side_mut(who);
        if side.peers.linked(peer, stand_in, now) {
            side.peers.reclaim(peer);
        }
        side.round = side.round.saturating_add(1);
        side.answered.clear();
        side.probes = 0;
    }

    /// Whether `who` holds a connection to the other host, alive this run,
    /// and up at its end.
    fn linked(&self, who: Who) -> bool {
        let life = self.side(who).life;
        self.conns.iter().any(|conn| {
            conn.alive(who, life)
                && if who == conn.client {
                    conn.client_up
                } else {
                    conn.server_up
                }
        })
    }

    /// Whether `who` holds a connection to the other host alive this run,
    /// up or still dialling.
    fn holds(&self, who: Who) -> bool {
        let life = self.side(who).life;
        self.conns.iter().any(|conn| conn.alive(who, life))
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "t={}ms laptop run {} at {}",
            self.now_ms, self.laptop.life, self.laptop.addr
        )?;
        for who in [Who::Laptop, Who::Desk] {
            let side = self.side(who);
            let other = who.other().id();
            writeln!(
                f,
                "  {who:?}: carrying {:?} trusted {:?} relay {:?} answered {:?} held {:?}",
                side.peers.carrying(other),
                side.peers.trusted_route(other),
                side.relay,
                side.answered,
                side.peers.held(),
            )?;
        }
        for conn in &self.conns {
            writeln!(
                f,
                "  conn {} {:?}#{}→{:?}#{} up {}/{} dead {}/{} remote {}/{:?} sent on {:?}",
                conn.id,
                conn.client,
                conn.client_life,
                conn.server,
                conn.server_life,
                conn.client_up,
                conn.server_up,
                conn.client_dead,
                conn.server_dead,
                conn.client_remote,
                conn.server_remote,
                conn.sent_on,
            )?;
        }
        writeln!(
            f,
            "  relay at {:?} holding {}",
            self.relay.at,
            self.relay.held.len()
        )?;
        for datagram in &self.in_flight {
            writeln!(
                f,
                "  in flight {} → {}: {:?}",
                datagram.from,
                datagram.to,
                parse(&datagram.bytes).map(short)
            )?;
        }
        if let Some(discarded) = &self.discarded {
            writeln!(f, "  discarded: {discarded}")?;
        }
        Ok(())
    }
}

fn short(kind: PacketKind<'_>) -> String {
    match kind {
        PacketKind::Quic(packet) => format!("{:?}", read_quic(packet)),
        PacketKind::Forwarded { host, payload } => {
            format!("to {} {:?}", name(host), parse(payload).map(short))
        }
        PacketKind::Delivered { host, payload } => {
            format!("from {} {:?}", name(host), parse(payload).map(short))
        }
        PacketKind::Probe { from, .. } => format!("probe from {}", name(from)),
        PacketKind::ProbeAnswer { from, .. } => format!("answer from {}", name(from)),
    }
}

fn name(host: HostId) -> &'static str {
    if host == LAPTOP_ID { "laptop" } else { "desk" }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Deliver(MsgIndex),
    Fault(Fault),
    /// The laptop's process is killed and started again on another port.
    RestartElsewhere,
    /// The laptop's relay dial says its Hello at the silent address.
    RelaySilent,
    /// It says it where the relay listens, and the relay admits it.
    RelayAdmitted,
    /// The laptop's link to the relay goes.
    RelayDown,
    /// The laptop dials the desk on every path it knows, its local
    /// address among them or not, and through the relay.
    Dial {
        direct: bool,
    },
    /// The laptop dials the desk through its relay link.
    DialThrough,
    /// One end of a connection sends.
    Send {
        conn: u8,
        client: bool,
    },
    /// A host's path machine probes the address it knows the other at.
    Probe(Who),
    /// A host's path machine moves the link to a path its probe answered.
    Carry(Who, Path),
    /// One end of a connection that can no longer reach the other times
    /// out.
    IdleOut {
        conn: u8,
        client: bool,
    },
    /// The client sends its first flight again for a packet that was lost.
    Again(u8),
    /// A dial that lost packets gives its handshake up, and the laptop may
    /// dial again.
    GiveUp(u8),
    /// The local network between the two goes.
    LanDown,
}

/// The bounds of one exploration.
#[derive(Clone, Copy, Debug)]
pub struct SocketModel {
    restarts: u8,
    elsewhere: bool,
    losses: u8,
    duplicates: u8,
    ticks: u8,
    relay_downs: u8,
    lan_down: bool,
    probes: u8,
    sends: u8,
}

impl SocketModel {
    /// The laptop started again once, on its port or another, with probes
    /// and stray packets from the dead run's link.
    pub fn ci() -> Self {
        Self {
            restarts: 1,
            elsewhere: true,
            losses: 0,
            duplicates: 0,
            ticks: 0,
            relay_downs: 0,
            lan_down: false,
            probes: 1,
            sends: 1,
        }
    }

    /// The relaunch on the laptop's port or another, with one datagram
    /// lost and one send on each link, for the nightly run.
    pub fn nightly() -> Self {
        Self {
            elsewhere: true,
            sends: 1,
            ..Self::loss()
        }
    }

    /// The local network goes under a live link, and the laptop dials the
    /// desk again through the relay.
    pub fn lan_down() -> Self {
        Self {
            restarts: 0,
            elsewhere: false,
            losses: 0,
            duplicates: 0,
            ticks: 0,
            relay_downs: 0,
            lan_down: true,
            probes: 0,
            sends: 0,
        }
    }

    /// A relaunch with one datagram lost, which QUIC sends again.
    pub fn loss() -> Self {
        Self {
            losses: 1,
            sends: 0,
            ..Self::quiet()
        }
    }

    /// A relaunch with one datagram delivered twice.
    pub fn duplicate() -> Self {
        Self {
            duplicates: 1,
            ..Self::quiet()
        }
    }

    /// The local network goes, the link moves to the relay, and the
    /// laptop's relay link goes and comes back under it.
    pub fn relay_down() -> Self {
        Self {
            relay_downs: 1,
            ..Self::lan_down()
        }
    }

    /// A relaunch with the clock passing the time unnamed senders are
    /// kept.
    pub fn time() -> Self {
        Self {
            ticks: 1,
            ..Self::quiet()
        }
    }

    /// A relaunch on the laptop's port with nothing else going wrong: the
    /// baseline each fault is explored against.
    fn quiet() -> Self {
        Self {
            restarts: 1,
            elsewhere: false,
            losses: 0,
            duplicates: 0,
            ticks: 0,
            relay_downs: 0,
            lan_down: false,
            probes: 0,
            sends: 1,
        }
    }

    /// The laptop's process starts afresh: its links die with it, the
    /// desk's ends of them do not.
    fn restart(&self, s: &mut State, addr: SocketAddr) {
        let life = s.laptop.life + 1;
        for conn in &mut s.conns {
            if conn.client == Who::Laptop && conn.client_life == s.laptop.life {
                conn.client_dead = true;
            }
            if conn.server == Who::Laptop && conn.server_life == s.laptop.life {
                conn.server_dead = true;
            }
        }
        s.laptop = Side::new(life, addr);
        s.restarts += 1;
    }

    /// The laptop dials the desk: the stand-in quinn dials, and its first
    /// flight.
    fn dial(&self, s: &mut State, stand_in: SocketAddr) {
        let id = s.conns.len() as u8;
        s.conns.push(Conn {
            id,
            client: Who::Laptop,
            client_life: s.laptop.life,
            server: Who::Desk,
            server_life: s.desk.life,
            client_remote: stand_in,
            server_remote: None,
            client_up: false,
            server_up: false,
            client_dead: false,
            server_dead: false,
            sent_on: BTreeSet::new(),
            lost: 0,
            again: 0,
            sent: 0,
            stranded: false,
        });
        s.laptop.dials += 1;
        s.transmit(Who::Laptop, stand_in, &quic(id, Quic::Initial), Some(id));
    }

    /// One end of a connection timed out: a host whose last link to the
    /// other went releases its route.
    fn idle_out(&self, s: &mut State, at: usize, client: bool) {
        let conn = &mut s.conns[at];
        let who = if client {
            conn.client_dead = true;
            conn.client
        } else {
            conn.server_dead = true;
            conn.server
        };
        if !s.holds(who) {
            let other = who.other().id();
            s.side_mut(who).peers.release(other);
        }
    }
}

impl Machine for SocketModel {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let mut s = State {
            laptop: Side::new(0, LAPTOP_ADDR),
            desk: Side::new(0, DESK_ADDR),
            relay: Relay::default(),
            conns: Vec::new(),
            in_flight: Vec::new(),
            now_ms: 0,
            restarts: 0,
            losses: 0,
            duplicates: 0,
            ticks: 0,
            relay_downs: 0,
            lan_down: false,
            gave_up: 0,
            discarded: None,
            sent_again: false,
        };
        // Both signed in to the relay.
        let via = s.desk.peers.stand_in_at(RELAY_ADDR, 0);
        s.desk.peers.add_forwarding(DESK_RELAY, via, Relayed);
        s.admit(Who::Desk);
        let link = LinkId(10);
        let via = s.laptop.peers.stand_in_at(RELAY_ADDR, 0);
        s.laptop.peers.add_forwarding(link, via, Relayed);
        s.laptop.relay = Some(RelayLink {
            link,
            tried_silent: false,
            admitted: true,
        });
        s.admit(Who::Laptop);
        // The laptop dials the desk on its local address, and the link
        // comes up there.
        let stand_in = s
            .laptop
            .peers
            .dial_on_all(DESK_ID, vec![Path::Direct(DESK_ADDR)], true, 0);
        self.dial(&mut s, stand_in);
        while let Some(at) = s
            .in_flight
            .iter()
            .position(|datagram| datagram.to != RELAY_ADDR)
        {
            let datagram = s.in_flight.remove(at);
            self.next_deliver(&mut s, datagram);
        }
        s.in_flight.clear();
        s.relay.held.clear();
        s.laptop.dials = 0;
        assert!(s.linked(Who::Laptop) && s.linked(Who::Desk), "{s:?}");
        s
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        out.extend((0..s.in_flight.len()).map(|at| Action::Deliver(MsgIndex(at))));
        if s.losses < self.losses {
            out.extend((0..s.in_flight.len()).map(|at| Action::Fault(Fault::Lose(MsgIndex(at)))));
        }
        if s.duplicates < self.duplicates {
            out.extend(
                (0..s.in_flight.len()).map(|at| Action::Fault(Fault::Duplicate(MsgIndex(at)))),
            );
        }
        // A relay dial gives up within its handshake timeout, long before
        // this much time passes, and dials afresh.
        let relay_dialling = s.laptop.relay.is_some_and(|relay| !relay.admitted);
        if s.ticks < self.ticks && !relay_dialling {
            out.push(Action::Fault(Fault::Tick {
                ms: PAST_UNCLAIMED_MS,
            }));
        }
        if s.restarts < self.restarts {
            out.push(Action::Fault(Fault::Crash(Host(0))));
            if self.elsewhere {
                out.push(Action::RestartElsewhere);
            }
        }
        match s.laptop.relay {
            None => {
                out.push(Action::RelaySilent);
                out.push(Action::RelayAdmitted);
            }
            Some(relay) if !relay.admitted => out.push(Action::RelayAdmitted),
            Some(_) if s.relay_downs < self.relay_downs => out.push(Action::RelayDown),
            Some(_) => {}
        }
        if !s.holds(Who::Laptop) && s.laptop.dials == 0 {
            out.push(Action::Dial { direct: true });
            out.push(Action::Dial { direct: false });
            // A dial through the relay follows the relay saying the desk
            // is there, which it does once it has admitted the link.
            if s.laptop.relay.is_some_and(|relay| relay.admitted) {
                out.push(Action::DialThrough);
            }
        }
        for conn in &s.conns {
            for client in [true, false] {
                let who = if client { conn.client } else { conn.server };
                let up = if client {
                    conn.client_up
                } else {
                    conn.server_up
                };
                let life = s.side(who).life;
                if !conn.alive(who, life) {
                    continue;
                }
                if up && conn.sent < self.sends {
                    out.push(Action::Send {
                        conn: conn.id,
                        client,
                    });
                }
                if !conn.alive(who.other(), s.side(who.other()).life) || conn.stranded {
                    out.push(Action::IdleOut {
                        conn: conn.id,
                        client,
                    });
                }
            }
            if conn.alive(conn.client, s.laptop.life) && !conn.client_up && conn.lost > 0 {
                // QUIC sends it again on a timer; once is enough to tell a
                // flight lost from one stranded where nothing answers.
                if conn.again < conn.lost && conn.again == 0 {
                    out.push(Action::Again(conn.id));
                }
                if s.gave_up == 0 {
                    out.push(Action::GiveUp(conn.id));
                }
            }
        }
        for who in [Who::Laptop, Who::Desk] {
            let side = s.side(who);
            if side.probes < self.probes && s.linked(who) {
                out.push(Action::Probe(who));
            }
            if s.linked(who) {
                let carrying = side.peers.carrying(who.other().id());
                out.extend(
                    side.answered
                        .iter()
                        .filter(|path| carrying != Some(**path))
                        .map(|path| Action::Carry(who, *path)),
                );
            }
        }
        if self.lan_down && !s.lan_down {
            out.push(Action::LanDown);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut s = s.clone();
        let now = s.now_ms;
        match *action {
            Action::Deliver(MsgIndex(at)) => {
                let datagram = s.in_flight.remove(at);
                self.next_deliver(&mut s, datagram);
            }
            Action::Fault(Fault::Lose(MsgIndex(at))) => {
                let datagram = s.in_flight.remove(at);
                s.losses += 1;
                let lost = parse(&datagram.bytes).ok().and_then(|kind| match kind {
                    PacketKind::Quic(packet) => read_quic(packet),
                    PacketKind::Forwarded { payload, .. }
                    | PacketKind::Delivered { payload, .. } => read_quic(payload),
                    _ => None,
                });
                if let Some((id, _)) = lost
                    && let Some(conn) = s.conns.iter_mut().find(|conn| conn.id == id)
                {
                    conn.lost += 1;
                }
            }
            Action::Fault(Fault::Duplicate(MsgIndex(at))) => {
                let datagram = s.in_flight[at].clone();
                s.duplicates += 1;
                s.send(datagram);
            }
            Action::Fault(Fault::Tick { ms }) => {
                // A live link to the relay is heard from far more often
                // than the time unnamed senders are kept: twice over the
                // tick here.
                for step in [ms / 2, ms - ms / 2] {
                    s.now_ms += step;
                    for who in [Who::Laptop, Who::Desk] {
                        if s.relay.at.contains_key(&who) {
                            let now = s.now_ms;
                            let heard = s
                                .side_mut(who)
                                .peers
                                .arrived_quic(Arrival::Udp(RELAY_ADDR), now);
                            s.heard(who, heard);
                        }
                    }
                }
                s.ticks += 1;
            }
            Action::Fault(Fault::Crash(_)) => {
                let addr = s.laptop.addr;
                self.restart(&mut s, addr);
            }
            Action::Fault(_) => return None,
            Action::RestartElsewhere => {
                let addr = if s.laptop.addr == LAPTOP_ADDR {
                    LAPTOP_MOVED
                } else {
                    LAPTOP_ADDR
                };
                self.restart(&mut s, addr);
            }
            Action::RelaySilent | Action::RelayAdmitted => {
                let link = LinkId(10 + u64::from(s.laptop.life) * 2 + u64::from(s.relay_downs));
                let admitted = *action == Action::RelayAdmitted;
                let at = if admitted { RELAY_ADDR } else { RELAY_SILENT };
                let via = s.laptop.peers.stand_in_at(at, now);
                let added = s.laptop.peers.add_forwarding(link, via, Relayed);
                let tried_silent =
                    !admitted || s.laptop.relay.is_some_and(|relay| relay.tried_silent);
                s.laptop.relay = Some(RelayLink {
                    link,
                    tried_silent,
                    admitted,
                });
                for again in added.again {
                    s.sent_again |= !again.sent.is_empty();
                    if let Ok(wire) = s.laptop.peers.wire(again.peer, again.path) {
                        for bytes in &again.sent {
                            s.record_flight(Who::Laptop, bytes, wire);
                            s.put(Who::Laptop, wire, bytes);
                        }
                    }
                }
                if admitted {
                    s.admit(Who::Laptop);
                }
            }
            Action::RelayDown => {
                let relay = s.laptop.relay.take()?;
                s.laptop.peers.remove_forwarding(relay.link);
                s.relay.at.remove(&Who::Laptop);
                s.relay_downs += 1;
            }
            Action::Dial { direct } => {
                let paths = if direct {
                    vec![Path::Direct(s.laptop.knows_at(Who::Laptop))]
                } else {
                    Vec::new()
                };
                let stand_in = s.laptop.peers.dial_on_all(DESK_ID, paths, true, now);
                self.dial(&mut s, stand_in);
            }
            Action::DialThrough => {
                let link = s.laptop.relay?.link;
                let stand_in = s.laptop.peers.dial_through(DESK_ID, link, now);
                self.dial(&mut s, stand_in);
            }
            Action::Send { conn, client } => {
                let at = s.conns.iter().position(|held| held.id == conn)?;
                s.conns[at].sent += 1;
                let held = s.conns[at].clone();
                let (who, remote) = if client {
                    (held.client, Some(held.client_remote))
                } else {
                    (held.server, held.server_remote)
                };
                s.transmit(who, remote?, &quic(conn, Quic::Data), None);
            }
            Action::Probe(who) => {
                let other = who.other();
                let path = Path::Direct(s.side(who).knows_at(who));
                let side = s.side_mut(who);
                side.probes += 1;
                let round = side.round;
                if let Ok(wire) = side.peers.wire(Peer::Host(other.id()), path) {
                    let WireTo::Udp(to) = wire.to else {
                        return None;
                    };
                    let from = side.addr;
                    s.send(Datagram {
                        from,
                        to,
                        bytes: encode_probe(who.id(), u64::from(round)).to_vec(),
                    });
                }
            }
            Action::Carry(who, path) => {
                let other = who.other().id();
                let side = s.side_mut(who);
                side.answered.remove(&path);
                side.peers.set_carrying(other, path, now);
            }
            Action::IdleOut { conn, client } => {
                let at = s.conns.iter().position(|held| held.id == conn)?;
                self.idle_out(&mut s, at, client);
            }
            Action::Again(conn) => {
                let at = s.conns.iter().position(|held| held.id == conn)?;
                s.conns[at].again += 1;
                let remote = s.conns[at].client_remote;
                s.transmit(Who::Laptop, remote, &quic(conn, Quic::Initial), Some(conn));
            }
            Action::GiveUp(conn) => {
                let at = s.conns.iter().position(|held| held.id == conn)?;
                s.conns[at].client_dead = true;
                s.laptop.dials = 0;
                s.gave_up += 1;
                if !s.holds(Who::Laptop) {
                    s.laptop.peers.release(DESK_ID);
                }
            }
            Action::LanDown => {
                s.lan_down = true;
                for conn in &mut s.conns {
                    conn.stranded = true;
                }
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(NEVER_DISCARDS, |_, s: &State| s.discarded.is_none()),
            Property::always(FLIGHT_FOLLOWS, |_, s: &State| {
                s.conns.iter().all(|conn| {
                    let side = s.side(conn.client);
                    if !conn.alive(conn.client, side.life) || conn.client_up {
                        return true;
                    }
                    let peer = conn.server.id();
                    match side.peers.carrying(peer) {
                        Some(path) => match side.peers.wire(Peer::Host(peer), path) {
                            Ok(wire) => conn.sent_on.contains(&wire),
                            Err(NoPath(_)) => true,
                        },
                        None => true,
                    }
                })
            }),
            Property::always(NO_GONE_FORWARDER, |_, s: &State| {
                [Who::Laptop, Who::Desk].into_iter().all(|who| {
                    let side = s.side(who);
                    match side.peers.carrying(who.other().id()) {
                        Some(Path::Forwarded(link)) => side.peers.forwarded_kind(link).is_some(),
                        _ => true,
                    }
                })
            }),
            Property::always(STRAYS_BOUNDED, |_, s: &State| {
                s.laptop.peers.held().0 <= HELD_PEERS && s.desk.peers.held().0 <= HELD_PEERS
            }),
            // Judged where the world has settled, with nothing left to
            // happen: the link the laptop had before its last restart, up
            // earlier on the path, says nothing of the dial since.
            Property::eventually(LINKS, |model: &SocketModel, s: &State| {
                let mut left = Vec::new();
                model.actions(s, &mut left);
                left.is_empty()
                    && s.linked(Who::Laptop)
                    && s.conns.iter().all(|conn| {
                        !conn.alive(conn.client, s.laptop.life)
                            || (conn.client_up && conn.server_up && !conn.server_dead)
                    })
            }),
            Property::sometimes(
                "a relaunched laptop sometimes links",
                |model: &SocketModel, s: &State| {
                    model.restarts == 0 || (s.restarts > 0 && s.linked(Who::Laptop))
                },
            ),
            Property::sometimes(
                "a first flight is sometimes sent again where it moved",
                |model: &SocketModel, s: &State| model.restarts == 0 || s.sent_again,
            ),
            Property::sometimes(
                "once the local network goes, the laptop sometimes links again through the relay",
                |model: &SocketModel, s: &State| {
                    !model.lan_down
                        || (s.lan_down
                            && s.linked(Who::Laptop)
                            && matches!(s.laptop.peers.carrying(DESK_ID), Some(Path::Forwarded(_))))
                },
            ),
        ]
    }
}

impl SocketModel {
    /// One datagram arrives where it was sent, if the network carries it
    /// and something listens there.
    fn next_deliver(&self, s: &mut State, datagram: Datagram) {
        if !s.carries(datagram.from, datagram.to) {
            return;
        }
        if datagram.to == RELAY_ADDR {
            s.at_relay(datagram.from, &datagram.bytes);
        } else if datagram.to == s.desk.addr {
            s.at_host(Who::Desk, datagram.from, &datagram.bytes);
        } else if datagram.to == s.laptop.addr {
            s.at_host(Who::Laptop, datagram.from, &datagram.bytes);
        }
    }
}

/// The most peers one socket holds here: the other host, the relay at its
/// two addresses, and an unnamed sender at each of the other host's.
const HELD_PEERS: usize = 6;

pub const NEVER_DISCARDS: &str =
    "a dialler never drops its peer's packets as coming from somewhere it does not know";
pub const FLIGHT_FOLLOWS: &str =
    "a dial's first flight has gone out on whatever path carries the dial";
pub const NO_GONE_FORWARDER: &str = "no route stays on a forwarding link that has gone";
pub const STRAYS_BOUNDED: &str =
    "a socket holds no more peers than the hosts and addresses it has heard from";
pub const LINKS: &str = "once the world settles, the laptop's dial has linked at both ends, sending again only what was lost";
