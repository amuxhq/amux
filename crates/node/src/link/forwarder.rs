//! The forwarder a relay and a daemon run between the links of their hosts.
//!
//! A host reaches a peer through a forwarder by sending its QUIC packets to
//! the forwarder behind a header naming the peer. The forwarder reads only
//! that header: it looks the sender's link and the destination up in its
//! table of pairs, rewrites the header to name the sender and puts the
//! packet on the destination's link. The table is filled and emptied by
//! the relay core alone ([`relay_core::Output::Forward`] and
//! [`relay_core::Output::Unforward`]); a packet for a pair the table does
//! not hold asks the core once, and the core either installs the pair or
//! refuses it, and a refusal is written on the sender's control link at
//! most once per [`REFUSAL_REPEAT_MS`] for each destination.
//!
//! A host sends its first packets for a peer in the same flight as the
//! handshake of its own link to the forwarder, so they arrive before the
//! forwarder knows who sent them. They are held per sender, a little and
//! briefly, and released through the table when the sender's link is
//! admitted, or dropped when it is not. Nothing is ever sent in answer to a
//! held packet: a sender not yet admitted could be lying about its address,
//! and an answer would be reflected at whoever owns it.
//!
//! Each admitted link may send so many packets and bytes a second; beyond
//! that its packets are dropped as a congested router would drop them, and
//! QUIC's congestion control on the sending host slows down.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use daemon_core::DumpReason;
use daemon_core::payload::ForwardRefusal;
pub use relay_core::REFUSAL_REPEAT_MS;
use relay_core::{Input, Output, Relay};
use tokio::time::Instant;

use super::socket::{LinkId, encode_delivered};
use crate::HostId;

/// What the forwarder keeps of packets from senders not yet admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldLimits {
    /// Packets held for one sender; its oldest goes first past this.
    pub per_sender_packets: usize,
    /// Bytes held for one sender; its oldest goes first past this.
    pub per_sender_bytes: usize,
    /// How long a packet is held before it is dropped unreleased.
    pub for_ms: u64,
    /// Bytes held for every sender together; the oldest held goes first.
    pub total_bytes: usize,
}

impl HoldLimits {
    /// Enough for a launch's first flight (a handshake and a few early
    /// calls) behind a link handshake that takes one round trip.
    pub const DEFAULT: Self = Self {
        per_sender_packets: 32,
        per_sender_bytes: 64 * 1024,
        for_ms: 1_000,
        total_bytes: 64 * 1024 * 1024,
    };
}

/// What one admitted link may send through the forwarder each second. A
/// link may spend a whole second's allowance at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkLimits {
    pub packets_per_s: u64,
    pub bytes_per_s: u64,
}

impl LinkLimits {
    /// Far above what one host's sessions send, so only a host gone wrong
    /// meets it: about 64 MiB a second in full-sized packets.
    pub const DEFAULT: Self = Self {
        packets_per_s: 56_000,
        bytes_per_s: 64 * 1024 * 1024,
    };
}

/// Where the forwarder's packets and refusals go: the driver's links.
pub trait ForwardLinks: Send + Sync + 'static {
    /// Puts one datagram on `link`'s path to its host. Queues and never
    /// waits; a link that cannot take it now drops it, as a full UDP
    /// buffer would.
    fn send(&self, link: LinkId, datagram: &[u8]) -> io::Result<()>;
    /// Writes `ForwardRefused` naming `dst` on `link`'s control stream.
    fn refuse(&self, link: LinkId, dst: HostId, reason: ForwardRefusal);
    /// The core asked for a dump while deciding a packet.
    fn dump(&self, reason: DumpReason);
    /// `input` is about to be stepped on `core`, under the core's lock:
    /// the driver's record of the core's recent inputs, for a dump.
    fn record(&self, core: &Relay, input: &Input) {
        let _ = (core, input);
    }
}

/// What became of one arriving packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forwarded {
    Sent,
    /// Kept until its sender is admitted or fails to be.
    Held,
    /// Dropped. The sender was told only if this was the first refusal of
    /// its destination within [`REFUSAL_REPEAT_MS`].
    Dropped(ForwardRefusal),
}

/// The packets of a pair go from `from` to `to` with the header rewritten
/// to `source`, the host `from` belongs to.
#[derive(Clone, Copy, Debug)]
struct Pair {
    to: LinkId,
    source: HostId,
}

struct HeldPacket {
    seq: u64,
    at_ms: u64,
    dst: HostId,
    payload: Vec<u8>,
}

#[derive(Default)]
struct SenderHold {
    packets: VecDeque<HeldPacket>,
    bytes: usize,
}

/// Packets from senders not yet admitted, per sender, with one queue of
/// every packet in arrival order so the oldest held anywhere goes first.
/// The order queue keeps entries of packets already released or dropped
/// until they reach its front; each packet is in it once, and each leaves
/// it within [`HoldLimits::for_ms`].
#[derive(Default)]
struct Hold {
    next_seq: u64,
    senders: HashMap<SocketAddr, SenderHold>,
    order: VecDeque<(u64, SocketAddr)>,
    bytes: usize,
    counts: Counts,
}

/// What a forwarder has done with the packets it was given, since it
/// started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Put on a link to their destination.
    pub forwarded: u64,
    /// Held from a sender not yet admitted.
    pub held: u64,
    /// Held, then let through the table on the sender's admission.
    pub released: u64,
    /// Held, then dropped unanswered: too old, crowded out, or the sender
    /// was not admitted.
    pub dropped: u64,
}

impl Hold {
    /// Drops `sender`'s oldest packet when it is the packet `seq`, or any
    /// when `seq` is `None`. Returns whether one went.
    fn drop_oldest(&mut self, sender: SocketAddr, seq: Option<u64>) -> bool {
        let Some(hold) = self.senders.get_mut(&sender) else {
            return false;
        };
        if seq.is_some_and(|seq| hold.packets.front().is_none_or(|held| held.seq != seq)) {
            return false;
        }
        let Some(held) = hold.packets.pop_front() else {
            return false;
        };
        hold.bytes -= held.payload.len();
        self.bytes -= held.payload.len();
        if hold.packets.is_empty() {
            self.senders.remove(&sender);
        }
        self.counts.dropped += 1;
        true
    }

    /// Drops every packet held past `for_ms`.
    fn expire(&mut self, now_ms: u64, for_ms: u64) {
        while let Some(&(seq, sender)) = self.order.front() {
            let expired = match self.senders.get(&sender).and_then(|h| h.packets.front()) {
                Some(held) if held.seq == seq => held.at_ms.saturating_add(for_ms) <= now_ms,
                // Released or dropped already.
                _ => true,
            };
            if !expired {
                break;
            }
            self.order.pop_front();
            self.drop_oldest(sender, Some(seq));
        }
    }

    /// Drops the oldest packet held for anyone.
    fn drop_oldest_anywhere(&mut self) {
        while let Some((seq, sender)) = self.order.pop_front() {
            if self.drop_oldest(sender, Some(seq)) {
                return;
            }
        }
    }

    fn take(&mut self, sender: SocketAddr) -> VecDeque<HeldPacket> {
        let Some(hold) = self.senders.remove(&sender) else {
            return VecDeque::new();
        };
        self.bytes -= hold.bytes;
        hold.packets
    }
}

/// A link's allowance, in thousandths of a packet and of a byte so a few
/// milliseconds still refill it.
struct Allowance {
    packets: u64,
    bytes: u64,
    at_ms: u64,
}

impl Allowance {
    fn full(limits: LinkLimits, now_ms: u64) -> Self {
        Self {
            packets: limits.packets_per_s.saturating_mul(1_000),
            bytes: limits.bytes_per_s.saturating_mul(1_000),
            at_ms: now_ms,
        }
    }

    fn spend(&mut self, limits: LinkLimits, now_ms: u64, len: usize) -> bool {
        let elapsed = now_ms.saturating_sub(self.at_ms);
        self.at_ms = self.at_ms.max(now_ms);
        let refill = |held: u64, per_s: u64| {
            held.saturating_add(per_s.saturating_mul(elapsed))
                .min(per_s.saturating_mul(1_000))
        };
        self.packets = refill(self.packets, limits.packets_per_s);
        self.bytes = refill(self.bytes, limits.bytes_per_s);
        let bytes = (len as u64).saturating_mul(1_000);
        if self.packets < 1_000 || self.bytes < bytes {
            return false;
        }
        self.packets -= 1_000;
        self.bytes -= bytes;
        true
    }
}

#[derive(Default)]
struct State {
    /// By the sending link and the destination host.
    pairs: HashMap<(LinkId, HostId), Pair>,
    /// When each sending link was last told a destination is refused, and
    /// why: its packets for it are dropped without asking the core again
    /// until [`REFUSAL_REPEAT_MS`] has passed.
    refused: HashMap<(LinkId, HostId), (ForwardRefusal, u64)>,
    allowances: HashMap<LinkId, Allowance>,
    hold: Hold,
    /// What was forwarded to a link the core paired before the driver
    /// could write to it, oldest first: the core pairs a link as it comes
    /// up, a moment before the driver registers its connection, and the
    /// first reply to a host's first flight lands in that moment.
    waiting: HashMap<LinkId, VecDeque<(u64, Vec<u8>)>>,
    /// The links the driver has registered, so a send that failed before
    /// one was registered and found it registered since is tried again
    /// rather than left waiting for an admission already past.
    registered: HashSet<LinkId>,
}

/// One forwarding output with the hosts it needs, read from the core in
/// the step that produced it.
enum Action {
    Forward {
        a: LinkId,
        a_host: HostId,
        b: LinkId,
        b_host: HostId,
    },
    Unforward {
        a: LinkId,
        a_up: bool,
        b: LinkId,
        b_up: bool,
        reason: ForwardRefusal,
        tell: bool,
    },
    Refuse {
        link: LinkId,
        dst: HostId,
        reason: ForwardRefusal,
    },
    Dump(DumpReason),
}

/// What a step leaves to do once the locks are released: the refusals to
/// write on links and the dumps to report.
#[derive(Default)]
struct Tell {
    refusals: Vec<(LinkId, HostId, ForwardRefusal)>,
    dumps: Vec<DumpReason>,
}

/// The forwarder a relay and a daemon run: pairs from the core, the header
/// rewrite, the hold buffer and per-link limits. Holding is the driver's;
/// releasing is the core's decision.
pub struct Forwarder {
    core: Arc<Mutex<Relay>>,
    links: Arc<dyn ForwardLinks>,
    hold_limits: HoldLimits,
    link_limits: LinkLimits,
    epoch: Instant,
    state: Mutex<State>,
}

impl Forwarder {
    pub fn new(
        core: Arc<Mutex<Relay>>,
        links: Arc<dyn ForwardLinks>,
        hold_limits: HoldLimits,
        link_limits: LinkLimits,
    ) -> Self {
        Self {
            core,
            links,
            hold_limits,
            link_limits,
            epoch: Instant::now(),
            state: Mutex::new(State::default()),
        }
    }

    /// The clock the forwarder steps the core by; a driver ticking the core
    /// itself must use this one.
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Steps the core, on this forwarder's clock, and acts on its
    /// forwarding outputs. Every output is returned; the forwarding ones are
    /// already done. A driver steps its core only through here.
    pub fn step(&self, input: Input) -> Vec<Output> {
        let gone = match input {
            Input::LinkDown { link } => Some(link),
            _ => None,
        };
        let (out, tell) = self.ask([
            Input::Tick {
                now_ms: self.now_ms(),
            },
            input,
        ]);
        let displaced = out.iter().filter_map(|output| match output {
            Output::Displace { link } => Some(*link),
            _ => None,
        });
        {
            let mut state = self.lock();
            for link in gone.into_iter().chain(displaced) {
                state.waiting.remove(&link);
                state.registered.remove(&link);
            }
        }
        self.tell(tell);
        out
    }

    /// Forwards, holds or drops one packet arriving on an admitted link,
    /// with its header already read: `dst` is the host it names and
    /// `payload` the QUIC packet behind it.
    pub fn on_packet(&self, from: LinkId, dst: HostId, payload: &[u8]) -> Forwarded {
        let now_ms = self.now_ms();
        let refused = {
            let mut state = self.lock();
            let limits = self.link_limits;
            let allowed = state
                .allowances
                .entry(from)
                .or_insert_with(|| Allowance::full(limits, now_ms))
                .spend(limits, now_ms, payload.len());
            if !allowed {
                return Forwarded::Dropped(ForwardRefusal::RateLimited);
            }
            if let Some(pair) = state.pairs.get(&(from, dst)).copied() {
                drop(state);
                return self.send(pair, payload);
            }
            match state.refused.get(&(from, dst)) {
                Some(&(reason, at)) if now_ms < at.saturating_add(REFUSAL_REPEAT_MS) => {
                    return Forwarded::Dropped(reason);
                }
                Some(&(reason, _)) => Some(reason),
                None => None,
            }
        };
        let (_, tell) = self.ask([
            Input::Tick { now_ms },
            Input::Packet {
                from,
                dst: core_host(dst),
            },
        ]);
        self.tell(tell);
        let state = self.lock();
        if let Some(pair) = state.pairs.get(&(from, dst)).copied() {
            drop(state);
            return self.send(pair, payload);
        }
        // Refused now, or refused within the window the core keeps (it may
        // have been told before this forwarder's cache saw it), or the
        // sender is not up in the core and so is told nothing.
        let reason = state
            .refused
            .get(&(from, dst))
            .map(|&(reason, _)| reason)
            .or(refused)
            .unwrap_or(ForwardRefusal::NoRoute);
        Forwarded::Dropped(reason)
    }

    /// Holds a packet from a sender whose link is not admitted yet, known
    /// only by the address it came from. Nothing is sent in answer, now or
    /// when it is dropped.
    pub fn hold(&self, sender: SocketAddr, dst: HostId, payload: &[u8]) -> Forwarded {
        let now_ms = self.now_ms();
        let limits = self.hold_limits;
        if payload.len() > limits.per_sender_bytes || payload.len() > limits.total_bytes {
            return Forwarded::Dropped(ForwardRefusal::RateLimited);
        }
        let mut state = self.lock();
        let hold = &mut state.hold;
        hold.expire(now_ms, limits.for_ms);
        loop {
            let full = hold.senders.get(&sender).is_some_and(|held| {
                held.packets.len() >= limits.per_sender_packets
                    || held.bytes + payload.len() > limits.per_sender_bytes
            });
            if !full {
                break;
            }
            hold.drop_oldest(sender, None);
        }
        while hold.bytes + payload.len() > limits.total_bytes {
            hold.drop_oldest_anywhere();
        }
        let seq = hold.next_seq;
        hold.next_seq += 1;
        let held = hold.senders.entry(sender).or_default();
        held.bytes += payload.len();
        held.packets.push_back(HeldPacket {
            seq,
            at_ms: now_ms,
            dst,
            payload: payload.to_vec(),
        });
        hold.bytes += payload.len();
        hold.order.push_back((seq, sender));
        hold.counts.held += 1;
        Forwarded::Held
    }

    /// The link from `sender` was admitted and is up in the core as `link`:
    /// its held packets go through the table as if they arrived on it now.
    pub fn on_admitted(&self, link: LinkId, sender: SocketAddr) {
        let now_ms = self.now_ms();
        let (held, waiting) = {
            let mut state = self.lock();
            state.hold.expire(now_ms, self.hold_limits.for_ms);
            let held = state.hold.take(sender);
            state.registered.insert(link);
            let waiting: Vec<Vec<u8>> = state
                .waiting
                .remove(&link)
                .unwrap_or_default()
                .into_iter()
                .filter(|(at_ms, _)| now_ms < at_ms.saturating_add(self.hold_limits.for_ms))
                .map(|(_, datagram)| datagram)
                .collect();
            state.hold.counts.released += (held.len() + waiting.len()) as u64;
            (held, waiting)
        };
        for datagram in waiting {
            let _ = self.links.send(link, &datagram);
        }
        for packet in held {
            self.on_packet(link, packet.dst, &packet.payload);
        }
    }

    /// The link from `sender` was not admitted: its held packets are
    /// dropped unanswered.
    pub fn on_admission_failed(&self, sender: SocketAddr) {
        let mut state = self.lock();
        let dropped = state.hold.take(sender).len() as u64;
        state.hold.counts.dropped += dropped;
    }

    /// What this forwarder has done with the packets it was given, counting
    /// what it has held past its time as dropped.
    pub fn counts(&self) -> Counts {
        let mut state = self.lock();
        state.hold.expire(self.now_ms(), self.hold_limits.for_ms);
        state.hold.counts
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send(&self, pair: Pair, payload: &[u8]) -> Forwarded {
        let datagram = encode_delivered(pair.source, payload);
        match self.links.send(pair.to, &datagram) {
            Ok(()) => {
                self.lock().hold.counts.forwarded += 1;
                Forwarded::Sent
            }
            // Paired, so up in the core: its connection is moments from
            // being registered, or its going is about to be stepped.
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                self.wait_for(pair.to, datagram)
            }
            Err(_) => Forwarded::Dropped(ForwardRefusal::NoRoute),
        }
    }

    /// Keeps `datagram` for `link` until the driver registers it, within
    /// the limits a sender's held packets keep to.
    fn wait_for(&self, link: LinkId, datagram: Vec<u8>) -> Forwarded {
        let now_ms = self.now_ms();
        let limits = self.hold_limits;
        let mut state = self.lock();
        if state.registered.contains(&link) {
            drop(state);
            return match self.links.send(link, &datagram) {
                Ok(()) => {
                    self.lock().hold.counts.forwarded += 1;
                    Forwarded::Sent
                }
                Err(_) => Forwarded::Dropped(ForwardRefusal::NoRoute),
            };
        }
        let State { waiting, hold, .. } = &mut *state;
        let queue = waiting.entry(link).or_default();
        let before = queue.len();
        queue.retain(|(at_ms, _)| now_ms < at_ms.saturating_add(limits.for_ms));
        let mut dropped = before - queue.len();
        let bytes =
            |queue: &VecDeque<(u64, Vec<u8>)>| queue.iter().map(|(_, d)| d.len()).sum::<usize>();
        while !queue.is_empty()
            && (queue.len() >= limits.per_sender_packets
                || bytes(queue) + datagram.len() > limits.per_sender_bytes)
        {
            queue.pop_front();
            dropped += 1;
        }
        hold.counts.dropped += dropped as u64;
        if datagram.len() > limits.per_sender_bytes {
            return Forwarded::Dropped(ForwardRefusal::NoRoute);
        }
        queue.push_back((now_ms, datagram));
        hold.counts.held += 1;
        Forwarded::Held
    }

    /// Steps the core and changes the table as it says, under the core's
    /// lock: two steps racing apply their outputs in the order the core
    /// produced them, so a pair a later step removed is never put back by
    /// an earlier one. What is written to links waits for the locks to go.
    fn ask(&self, inputs: impl IntoIterator<Item = Input>) -> (Vec<Output>, Tell) {
        let mut core = self
            .core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut out = Vec::new();
        let mut learned = Vec::new();
        for input in inputs {
            self.links.record(&core, &input);
            learned.push(Learned::of(&input));
            core.step(input, &mut out);
        }
        let host_of = |link: LinkId| core.host_of(link).map(node_host);
        let actions: Vec<Action> = out
            .iter()
            .filter_map(|output| match *output {
                Output::Forward { a, b } => Some(Action::Forward {
                    a,
                    a_host: host_of(a)?,
                    b,
                    b_host: host_of(b)?,
                }),
                Output::Unforward {
                    a,
                    b,
                    reason,
                    tell_both,
                } => Some(Action::Unforward {
                    a,
                    a_up: host_of(a).is_some(),
                    b,
                    b_up: host_of(b).is_some(),
                    reason,
                    tell: tell_both,
                }),
                Output::Refuse { link, dst, reason } => Some(Action::Refuse {
                    link,
                    dst: node_host(dst),
                    reason,
                }),
                Output::Dump { ref reason } => Some(Action::Dump(reason.clone())),
                _ => None,
            })
            .collect();
        let tell = self.act(learned, actions);
        drop(core);
        (out, tell)
    }

    /// Changes the table as the core said, with the core's lock held.
    fn act(&self, learned: Vec<Learned>, actions: Vec<Action>) -> Tell {
        let now_ms = self.now_ms();
        let mut tell = Tell::default();
        let mut state = self.lock();
        // What the core learned may turn a refusal into a pair: the core is
        // asked afresh, and the repeat window only spares the sender being
        // told again.
        for learned in learned {
            match learned {
                Learned::Nothing => {}
                Learned::Host(host) => state.refused.retain(|(_, dst), _| *dst != host),
                Learned::Plan => state.refused.clear(),
                Learned::LinkDown(link) => {
                    state.allowances.remove(&link);
                    state.refused.retain(|(from, _), _| *from != link);
                }
            }
        }
        for action in actions {
            match action {
                Action::Forward {
                    a,
                    a_host,
                    b,
                    b_host,
                } => {
                    state.pairs.insert(
                        (a, b_host),
                        Pair {
                            to: b,
                            source: a_host,
                        },
                    );
                    state.pairs.insert(
                        (b, a_host),
                        Pair {
                            to: a,
                            source: b_host,
                        },
                    );
                    state.refused.remove(&(a, b_host));
                    state.refused.remove(&(b, a_host));
                }
                Action::Unforward {
                    a,
                    a_up,
                    b,
                    b_up,
                    reason,
                    tell: told,
                } => {
                    let mut ends = Vec::new();
                    state.pairs.retain(|(from, dst), pair| {
                        let through = (*from == a && pair.to == b) || (*from == b && pair.to == a);
                        if through {
                            ends.push((*from, *dst));
                        }
                        !through
                    });
                    if !told {
                        continue;
                    }
                    for (end, dst) in ends {
                        let up = if end == a { a_up } else { b_up };
                        if up {
                            state.refused.insert((end, dst), (reason, now_ms));
                            tell.refusals.push((end, dst, reason));
                        }
                    }
                }
                Action::Refuse { link, dst, reason } => {
                    state.refused.insert((link, dst), (reason, now_ms));
                    tell.refusals.push((link, dst, reason));
                }
                Action::Dump(reason) => tell.dumps.push(reason),
            }
        }
        tell
    }

    /// Writes what a step left to tell, with no lock held.
    fn tell(&self, tell: Tell) {
        for (link, dst, reason) in tell.refusals {
            self.links.refuse(link, dst, reason);
        }
        for reason in tell.dumps {
            self.links.dump(reason);
        }
    }

    /// The pairs the table holds, as `(from, to)`.
    #[cfg(test)]
    fn table(&self) -> Vec<(LinkId, LinkId)> {
        self.lock()
            .pairs
            .iter()
            .map(|((from, _), pair)| (*from, pair.to))
            .collect()
    }
}

/// What an input tells the forwarder beyond the core's outputs.
enum Learned {
    Nothing,
    /// A link of this host came up: a destination refused before may be
    /// reachable now.
    Host(HostId),
    /// A plan changed: any refusal may be a pair now.
    Plan,
    /// The link is gone: its allowance and refusals with it.
    LinkDown(LinkId),
}

impl Learned {
    fn of(input: &Input) -> Self {
        match *input {
            Input::LinkUp { host, .. } => Self::Host(node_host(host)),
            Input::TierChanged { .. } => Self::Plan,
            Input::LinkDown { link } => Self::LinkDown(link),
            _ => Self::Nothing,
        }
    }
}

fn core_host(host: HostId) -> daemon_core::HostId {
    daemon_core::HostId(host.into_bytes())
}

fn node_host(host: daemon_core::HostId) -> HostId {
    HostId::from_bytes(host.0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use relay_core::{Tier, UserId};

    use super::*;

    const ALICE: UserId = UserId([1; 16]);
    const BOB: UserId = UserId([2; 16]);
    const DESK: HostId = HostId::from_u128(0xd);
    const LAPTOP: HostId = HostId::from_u128(0x1a);
    const PHONE: HostId = HostId::from_u128(0xf);
    const RELAY: HostId = HostId::from_u128(0x7e);
    const QUIC: &[u8] = &[0x40, 1, 2, 3];

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Wrote {
        Sent(LinkId, Vec<u8>),
        Refused(LinkId, HostId, ForwardRefusal),
    }

    #[derive(Default)]
    struct Links {
        wrote: Mutex<Vec<Wrote>>,
        dumps: Mutex<Vec<DumpReason>>,
        recorded: Mutex<Vec<Input>>,
        /// Links up in the core whose connections the driver has yet to
        /// register: a send to one fails as the relay's does.
        unregistered: Mutex<Vec<LinkId>>,
    }

    impl Links {
        fn take(&self) -> Vec<Wrote> {
            std::mem::take(&mut *self.wrote.lock().unwrap())
        }
    }

    impl ForwardLinks for Links {
        fn send(&self, link: LinkId, datagram: &[u8]) -> io::Result<()> {
            if self.unregistered.lock().unwrap().contains(&link) {
                return Err(io::Error::new(io::ErrorKind::NotConnected, "unregistered"));
            }
            self.wrote
                .lock()
                .unwrap()
                .push(Wrote::Sent(link, datagram.to_vec()));
            Ok(())
        }

        fn refuse(&self, link: LinkId, dst: HostId, reason: ForwardRefusal) {
            self.wrote
                .lock()
                .unwrap()
                .push(Wrote::Refused(link, dst, reason));
        }

        fn dump(&self, reason: DumpReason) {
            self.dumps.lock().unwrap().push(reason);
        }

        fn record(&self, _core: &Relay, input: &Input) {
            self.recorded.lock().unwrap().push(input.clone());
        }
    }

    struct Rig {
        forwarder: Forwarder,
        links: Arc<Links>,
        core: Arc<Mutex<Relay>>,
    }

    fn rig_with(hold: HoldLimits, limits: LinkLimits) -> Rig {
        let core = Arc::new(Mutex::new(Relay::new(core_host(RELAY))));
        let links = Arc::new(Links::default());
        let forwarder = Forwarder::new(core.clone(), links.clone(), hold, limits);
        Rig {
            forwarder,
            links,
            core,
        }
    }

    fn rig() -> Rig {
        rig_with(HoldLimits::DEFAULT, LinkLimits::DEFAULT)
    }

    impl Rig {
        fn up(&self, link: u64, user: UserId, host: HostId, tier: Option<Tier>) {
            self.forwarder.step(Input::LinkUp {
                link: LinkId(link),
                user,
                host: core_host(host),
                tier,
                instance: [0; 16],
                direct: None,
                advertised: Vec::new(),
            });
        }

        fn packet(&self, from: u64, dst: HostId, payload: &[u8]) -> Forwarded {
            self.forwarder.on_packet(LinkId(from), dst, payload)
        }

        fn pairs(&self) -> Vec<(LinkId, LinkId)> {
            self.core.lock().unwrap().pairs().collect()
        }
    }

    fn sent(link: u64, source: HostId, payload: &[u8]) -> Wrote {
        Wrote::Sent(LinkId(link), encode_delivered(source, payload))
    }

    fn refused(link: u64, dst: HostId, reason: ForwardRefusal) -> Wrote {
        Wrote::Refused(LinkId(link), dst, reason)
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, 1], port))
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_forwards_both_ways_naming_the_sender() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));

        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        assert_eq!(rig.pairs(), vec![(LinkId(1), LinkId(2))]);
        // The way back is the same pair: the core is not asked again.
        assert_eq!(rig.packet(2, DESK, b"back"), Forwarded::Sent);
        assert_eq!(rig.packet(1, LAPTOP, b"again"), Forwarded::Sent);
        assert_eq!(
            rig.links.take(),
            vec![
                sent(2, DESK, QUIC),
                sent(1, LAPTOP, b"back"),
                sent(2, DESK, b"again"),
            ]
        );
    }

    /// A lapse stepped while a packet on the same pair asks the core: the
    /// table changes in the order the core decided, so whichever came
    /// first, the lapsed pair is not left forwarding.
    #[test]
    fn forwarder_lapse_racing_a_packet_never_leaves_the_pair_in_the_table() {
        for _ in 0..2_000 {
            let rig = rig();
            rig.up(1, ALICE, DESK, Some(Tier::Pro));
            rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
            let start = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    start.wait();
                    rig.packet(1, LAPTOP, QUIC);
                });
                scope.spawn(|| {
                    start.wait();
                    rig.forwarder.step(Input::TierChanged {
                        link: LinkId(2),
                        tier: Tier::Free,
                    });
                });
            });
            assert!(rig.pairs().is_empty(), "the core holds no pair");
            assert_eq!(rig.forwarder.table(), vec![], "nor does the table");
            assert_eq!(
                rig.packet(1, LAPTOP, QUIC),
                Forwarded::Dropped(ForwardRefusal::PaymentRequired)
            );
        }
    }

    /// A destination refused because it had no link here is reachable as
    /// soon as it connects: its link coming up clears the refusal, and the
    /// repeat window spares only the telling.
    #[tokio::test(start_paused = true)]
    async fn forwarder_reaches_a_host_connecting_inside_the_refusal_window() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );
        assert_eq!(
            rig.links.take(),
            vec![refused(1, LAPTOP, ForwardRefusal::NoRoute)]
        );
        tokio::time::advance(Duration::from_millis(10)).await;
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        assert_eq!(rig.links.take(), vec![sent(2, DESK, QUIC)]);
    }

    /// Every input the core is stepped with is recorded for a dump, the
    /// ticks and the packets that ask it included.
    #[tokio::test(start_paused = true)]
    async fn forwarder_records_every_input_it_steps() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        let recorded = rig.links.recorded.lock().unwrap().clone();
        let kinds: Vec<&str> = recorded
            .iter()
            .map(|input| match input {
                Input::Tick { .. } => "tick",
                Input::LinkUp { .. } => "up",
                Input::Packet { .. } => "packet",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["tick", "up", "tick", "up", "tick", "packet"]);
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_daemon_forwards_between_pinned_links() {
        let rig = rig();
        rig.up(1, UserId::default(), DESK, None);
        rig.up(2, UserId::default(), PHONE, None);
        assert_eq!(rig.packet(2, DESK, QUIC), Forwarded::Sent);
        assert_eq!(rig.links.take(), vec![sent(1, PHONE, QUIC)]);
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_lapse_empties_the_table_and_tells_both_ends_once() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        rig.links.take();

        rig.forwarder.step(Input::TierChanged {
            link: LinkId(1),
            tier: Tier::Free,
        });
        assert!(rig.pairs().is_empty());
        let mut told = rig.links.take();
        told.sort_by_key(|wrote| format!("{wrote:?}"));
        assert_eq!(
            told,
            vec![
                refused(1, LAPTOP, ForwardRefusal::PaymentRequired),
                refused(2, DESK, ForwardRefusal::PaymentRequired),
            ]
        );

        // Both ends keep sending; neither is told again within the window.
        for _ in 0..3 {
            assert_eq!(
                rig.packet(1, LAPTOP, QUIC),
                Forwarded::Dropped(ForwardRefusal::PaymentRequired)
            );
            assert_eq!(
                rig.packet(2, DESK, QUIC),
                Forwarded::Dropped(ForwardRefusal::PaymentRequired)
            );
        }
        assert_eq!(rig.links.take(), vec![]);
    }

    /// A host refused while its plan was free is forwarded for as soon as
    /// the plan is Pro, not only once the refusal's repeat window is over.
    #[tokio::test(start_paused = true)]
    async fn forwarder_forwards_at_once_once_a_refused_plan_is_pro() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Free));
        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::PaymentRequired)
        );
        rig.forwarder.step(Input::TierChanged {
            link: LinkId(2),
            tier: Tier::Pro,
        });
        rig.links.take();
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        assert_eq!(rig.links.take(), vec![sent(2, DESK, QUIC)]);
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_link_down_tells_only_the_end_still_up() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        rig.links.take();

        rig.forwarder.step(Input::LinkDown { link: LinkId(2) });
        assert_eq!(
            rig.links.take(),
            vec![refused(1, LAPTOP, ForwardRefusal::NoRoute)]
        );
        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );
        assert_eq!(rig.links.take(), vec![]);
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_moves_a_pair_to_the_hosts_other_link_unannounced() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        rig.up(3, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        let first = match rig.links.take().as_slice() {
            [Wrote::Sent(link, _)] => *link,
            other => panic!("one packet sent, got {other:?}"),
        };
        let other = if first == LinkId(2) { 3 } else { 2 };

        rig.forwarder.step(Input::LinkDown { link: first });
        assert_eq!(rig.links.take(), vec![], "nobody is told of a move");
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        assert_eq!(rig.links.take(), vec![sent(other, DESK, QUIC)]);
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_repeats_a_refusal_only_after_the_window() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, BOB, LAPTOP, Some(Tier::Pro));

        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );
        assert_eq!(
            rig.links.take(),
            vec![refused(1, LAPTOP, ForwardRefusal::NoRoute)]
        );
        for _ in 0..10 {
            rig.packet(1, LAPTOP, QUIC);
        }
        tokio::time::advance(Duration::from_millis(REFUSAL_REPEAT_MS - 1)).await;
        rig.packet(1, LAPTOP, QUIC);
        assert_eq!(rig.links.take(), vec![], "not told twice in the window");

        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );
        assert_eq!(
            rig.links.take(),
            vec![refused(1, LAPTOP, ForwardRefusal::NoRoute)]
        );
        // Each destination has its own window.
        assert_eq!(
            rig.packet(1, PHONE, QUIC),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );
        assert_eq!(
            rig.links.take(),
            vec![refused(1, PHONE, ForwardRefusal::NoRoute)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_answers_nothing_to_a_link_not_up() {
        let rig = rig();
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(
            rig.packet(9, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );
        assert_eq!(rig.links.take(), vec![]);
        assert!(rig.pairs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_holds_then_releases_on_admission() {
        let rig = rig();
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        let sender = addr(4000);

        assert_eq!(rig.forwarder.hold(sender, LAPTOP, b"one"), Forwarded::Held);
        assert_eq!(rig.forwarder.hold(sender, LAPTOP, b"two"), Forwarded::Held);
        assert_eq!(rig.links.take(), vec![], "nothing answers a held packet");
        assert!(rig.pairs().is_empty(), "nothing before admission");

        tokio::time::advance(Duration::from_millis(500)).await;
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.forwarder.on_admitted(LinkId(1), sender);
        assert_eq!(
            rig.links.take(),
            vec![sent(2, DESK, b"one"), sent(2, DESK, b"two")]
        );
        // Released once.
        rig.forwarder.on_admitted(LinkId(1), sender);
        assert_eq!(rig.links.take(), vec![]);
        assert_eq!(
            rig.forwarder.counts(),
            Counts {
                forwarded: 2,
                held: 2,
                released: 2,
                dropped: 0
            }
        );
    }

    /// The core pairs a link as it comes up, a moment before the driver
    /// registers its connection: what is forwarded to it in that moment
    /// waits for it rather than being lost, so a relaunched host's first
    /// flight is answered at once instead of after its retransmit timer.
    #[tokio::test(start_paused = true)]
    async fn forwarder_keeps_what_arrives_for_a_link_until_it_is_registered() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.links.unregistered.lock().unwrap().push(LinkId(2));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));

        assert_eq!(rig.packet(1, LAPTOP, b"reply"), Forwarded::Held);
        assert_eq!(rig.links.take(), vec![]);

        rig.links.unregistered.lock().unwrap().clear();
        rig.forwarder.on_admitted(LinkId(2), addr(4000));
        assert_eq!(rig.links.take(), vec![sent(2, DESK, b"reply")]);
        // Registered, a send that failed before it was goes straight on.
        rig.links.unregistered.lock().unwrap().push(LinkId(2));
        assert_eq!(
            rig.packet(1, LAPTOP, b"gone"),
            Forwarded::Dropped(ForwardRefusal::NoRoute)
        );

        // What waits for a link that never registers goes with it.
        rig.links.unregistered.lock().unwrap().push(LinkId(3));
        rig.up(3, ALICE, PHONE, Some(Tier::Pro));
        assert_eq!(rig.packet(1, PHONE, b"lost"), Forwarded::Held);
        rig.forwarder.step(Input::LinkDown { link: LinkId(3) });
        rig.links.unregistered.lock().unwrap().clear();
        rig.forwarder.on_admitted(LinkId(3), addr(4001));
        assert_eq!(
            rig.links.take(),
            vec![refused(1, PHONE, ForwardRefusal::NoRoute)],
            "only the going told, nothing released"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_releases_a_refused_packet_to_an_admitted_link_only() {
        let rig = rig();
        rig.up(2, BOB, LAPTOP, Some(Tier::Pro));
        let sender = addr(4000);
        rig.forwarder.hold(sender, LAPTOP, QUIC);
        assert_eq!(rig.links.take(), vec![]);

        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.forwarder.on_admitted(LinkId(1), sender);
        assert_eq!(
            rig.links.take(),
            vec![refused(1, LAPTOP, ForwardRefusal::NoRoute)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_drops_held_packets_on_failure_and_after_a_second() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));

        let failed = addr(4000);
        rig.forwarder.hold(failed, LAPTOP, QUIC);
        rig.forwarder.on_admission_failed(failed);
        rig.forwarder.on_admitted(LinkId(1), failed);
        assert_eq!(rig.links.take(), vec![], "dropped on failure, unanswered");

        let slow = addr(4001);
        rig.forwarder.hold(slow, LAPTOP, b"early");
        tokio::time::advance(Duration::from_millis(600)).await;
        rig.forwarder.hold(slow, LAPTOP, b"late");
        tokio::time::advance(Duration::from_millis(400)).await;
        rig.forwarder.on_admitted(LinkId(1), slow);
        assert_eq!(
            rig.links.take(),
            vec![sent(2, DESK, b"late")],
            "held one second, no longer"
        );
        assert_eq!(
            rig.forwarder.counts(),
            Counts {
                forwarded: 1,
                held: 3,
                released: 1,
                dropped: 2
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_hold_drops_a_senders_oldest_past_its_limits() {
        let rig = rig();
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));

        let many = addr(4000);
        for n in 0..=32u8 {
            assert_eq!(rig.forwarder.hold(many, LAPTOP, &[n]), Forwarded::Held);
        }
        rig.forwarder.on_admitted(LinkId(1), many);
        let released = rig.links.take();
        assert_eq!(released.len(), 32);
        assert_eq!(released[0], sent(2, DESK, &[1]), "the oldest went");

        let large = addr(4001);
        let quarter = vec![7; 16 * 1024];
        for _ in 0..4 {
            rig.forwarder.hold(large, LAPTOP, &quarter);
        }
        rig.forwarder.hold(large, LAPTOP, b"last");
        rig.forwarder.on_admitted(LinkId(1), large);
        let released = rig.links.take();
        assert_eq!(released.len(), 4, "64 KB per sender");
        assert_eq!(released[3], sent(2, DESK, b"last"));

        assert_eq!(
            rig.forwarder
                .hold(addr(4002), LAPTOP, &vec![0; 64 * 1024 + 1]),
            Forwarded::Dropped(ForwardRefusal::RateLimited)
        );
        assert_eq!(rig.links.take(), vec![]);
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_hold_drops_the_oldest_anywhere_past_the_total() {
        let rig = rig_with(
            HoldLimits {
                total_bytes: 3,
                ..HoldLimits::DEFAULT
            },
            LinkLimits::DEFAULT,
        );
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));
        rig.up(3, ALICE, PHONE, Some(Tier::Pro));

        let (first, second) = (addr(4000), addr(4001));
        rig.forwarder.hold(first, LAPTOP, b"a");
        rig.forwarder.hold(second, LAPTOP, b"b");
        rig.forwarder.hold(first, LAPTOP, b"c");
        rig.forwarder.hold(second, LAPTOP, b"d");

        rig.forwarder.on_admitted(LinkId(1), first);
        rig.forwarder.on_admitted(LinkId(3), second);
        assert_eq!(
            rig.links.take(),
            vec![
                sent(2, DESK, b"c"),
                sent(2, PHONE, b"b"),
                sent(2, PHONE, b"d"),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_drops_a_link_over_its_limits_untold() {
        let rig = rig_with(
            HoldLimits::DEFAULT,
            LinkLimits {
                packets_per_s: 3,
                bytes_per_s: 1_000,
            },
        );
        rig.up(1, ALICE, DESK, Some(Tier::Pro));
        rig.up(2, ALICE, LAPTOP, Some(Tier::Pro));

        for _ in 0..3 {
            assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        }
        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::RateLimited)
        );
        // Per link: the other end still has its allowance.
        assert_eq!(rig.packet(2, DESK, QUIC), Forwarded::Sent);
        assert_eq!(rig.links.take().len(), 4, "the drop is not answered");

        tokio::time::advance(Duration::from_millis(334)).await;
        assert_eq!(rig.packet(1, LAPTOP, QUIC), Forwarded::Sent);
        assert_eq!(
            rig.packet(1, LAPTOP, QUIC),
            Forwarded::Dropped(ForwardRefusal::RateLimited)
        );

        // Bytes count as well as packets.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(rig.packet(1, LAPTOP, &[0; 900]), Forwarded::Sent);
        assert_eq!(
            rig.packet(1, LAPTOP, &[0; 200]),
            Forwarded::Dropped(ForwardRefusal::RateLimited)
        );
    }
}
