//! The driver of the forwarding core: the relay's and each daemon's.
//!
//! [`relay_core::Relay`] decides which pairs of links forward packets to
//! each other and what every link is told of its neighbors. Once the
//! profile's link socket is handed over, the core is stepped through the
//! [`Forwarder`], which carries the packets between the pairs it installs.
//! A relay shares one core across every account; a daemon has one of its
//! own, forwarding between its paired hosts.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use core_ring::DumpLimit;
use daemon_core::DumpReason;
use daemon_core::payload::ForwardRefusal;
use relay_core::{Input, Output, Relay};
use wire::pb;

use super::forwarder::{ForwardLinks, Forwarder, HoldLimits, LinkLimits};
use super::link_log::PathKind;
use super::socket::{ForwardSink, LinkSocket};
use crate::HostId;
use crate::routing::LinkOutputTx;
use crate::window::{self, Window};

/// Prefixes the reason the forwarding core asked for a dump.
pub const RELAY_CORE_DUMP: &str = "the relay core: ";

/// The forwarding core, and on a relay the forwarder its pairs drive.
pub(crate) struct Relaying {
    core: Arc<Mutex<Relay>>,
    /// Set on a relay, whose links carry forwarded packets and no streams.
    forwarding: OnceLock<Forwarding>,
    history: Mutex<History>,
    /// The clock the core is ticked by when nothing forwards here.
    epoch: Instant,
    /// Where a dump the core asks for is written, and where the check it
    /// broke is counted; unset only in tests that never read one.
    reports: OnceLock<(PathBuf, analytics::Analytics)>,
}

/// What a dump is made of: the core's recent inputs, and which dumps were
/// already reported this interval.
#[derive(Default)]
struct History {
    window: Window<Relay, Input>,
    dumps: Dumps,
}

/// The forwarder on a relay's link socket and the links it puts packets
/// on.
struct Forwarding {
    forwarder: Arc<Forwarder>,
    links: Arc<RelayLinks>,
}

/// The dumps the relay core asked for, reported once per kind in each
/// interval: on a busy relay one broken invariant or slow step comes again
/// with every stream.
#[derive(Debug, Default)]
struct Dumps(DumpLimit);

impl Dumps {
    /// The reason to report for `reason` asked for at `now_ms`, or `None`
    /// when it is held back.
    fn reason(&mut self, reason: &DumpReason, now_ms: u64) -> Option<core_ring::Admitted> {
        self.0.reason(RELAY_CORE_DUMP, reason, now_ms)
    }
}

impl Relaying {
    pub(crate) fn new(own: HostId) -> Arc<Self> {
        Arc::new(Self {
            core: Arc::new(Mutex::new(Relay::new(daemon_core::HostId(
                own.into_bytes(),
            )))),
            forwarding: OnceLock::new(),
            history: Mutex::default(),
            epoch: Instant::now(),
            reports: OnceLock::new(),
        })
    }

    /// Writes the dumps the core asks for into `dir`: a relay's directory
    /// from its configuration, a daemon's its installation's reports. A
    /// daemon's relay core counts each broken check on its profile's
    /// analytics; a relay's own server has none.
    pub(crate) fn report_to(&self, dir: PathBuf, analytics: analytics::Analytics) {
        let _ = self.reports.set((dir, analytics));
    }

    /// Forwards the packets hosts send `socket` to forward, between the
    /// pairs the core installs. From then on the core is stepped through
    /// the forwarder, and no stream is spliced.
    pub(crate) fn forward_on(self: &Arc<Self>, socket: Arc<LinkSocket>) {
        let links = Arc::new(RelayLinks {
            socket: Arc::downgrade(&socket),
            relaying: Arc::downgrade(self),
            admitted: Mutex::default(),
        });
        let forwarder = Arc::new(Forwarder::new(
            self.core.clone(),
            links.clone(),
            HoldLimits::DEFAULT,
            LinkLimits::DEFAULT,
        ));
        socket.forward_to(Arc::new(Arrivals {
            forwarder: forwarder.clone(),
            links: links.clone(),
        }));
        let _ = self.forwarding.set(Forwarding { forwarder, links });
    }

    /// What the forwarder has done with the packets it was given, once
    /// this forwards.
    pub(crate) fn forward_counts(&self) -> Option<super::forwarder::Counts> {
        self.forwarding
            .get()
            .map(|forwarding| forwarding.forwarder.counts())
    }

    /// The core this relays with, for a test that must make it ask for a
    /// dump.
    #[cfg(test)]
    fn with_relay(own: HostId, relay: Relay) -> Arc<Self> {
        let relaying = Self::new(own);
        *lock(&relaying.core) = relay;
        relaying
    }

    /// Steps each input in turn, through the forwarder where there is one,
    /// and reports the dumps the core asks for. Every other output is
    /// returned for the caller to act on.
    pub(crate) fn step(&self, inputs: impl IntoIterator<Item = Input>) -> Vec<Output> {
        let mut out = Vec::new();
        for input in inputs {
            let gone = match input {
                Input::LinkDown { link } => Some(link),
                _ => None,
            };
            match self.forwarding.get() {
                // The forwarder has recorded the input with the tick it
                // steps first, and reported the dumps among the outputs.
                Some(forwarding) => out.extend(
                    forwarding
                        .forwarder
                        .step(input)
                        .into_iter()
                        .filter(|output| !matches!(output, Output::Dump { .. })),
                ),
                None => {
                    let mut stepped = Vec::new();
                    {
                        let mut core = lock(&self.core);
                        self.record(&core, &input);
                        core.step(input, &mut stepped);
                    }
                    for output in stepped {
                        match output {
                            Output::Dump { reason } => self.report(&reason),
                            output => out.push(output),
                        }
                    }
                }
            }
            if let Some(forwarding) = self.forwarding.get() {
                let displaced = out.iter().filter_map(|output| match output {
                    Output::Displace { link } => Some(*link),
                    _ => None,
                });
                for link in gone.into_iter().chain(displaced) {
                    forwarding.links.forget(link);
                }
            }
        }
        out
    }

    /// A link to `host` the core holds as up runs on `carrier`, a
    /// connection on the profile's socket: its packets are forwarded from
    /// now on, those held for it first, and refusals are written to `tx`.
    /// A link the far end dialled names its sender's stand-in by `host`, so
    /// the host's packets are known by it whichever address they come from
    /// next.
    ///
    /// `through` says the link reaches a forwarder this host sends through:
    /// the relay, or a paired daemon. Its forwarded packets are this host's
    /// own from then on, carried on a path of that kind.
    pub(crate) fn admitted(
        &self,
        link: daemon_core::LinkId,
        tx: LinkOutputTx,
        host: HostId,
        carrier: Arc<dyn Seen>,
        through: Option<PathKind>,
    ) {
        let (Some(forwarding), Some(stand_in)) = (self.forwarding.get(), carrier.remote_address())
        else {
            return;
        };
        if let Some(socket) = forwarding.links.socket.upgrade() {
            if carrier.accepted() {
                socket.name(stand_in, host);
            }
            if let Some(kind) = through {
                socket.add_forwarding(link, stand_in, kind);
            }
        }
        forwarding.links.admit(
            link,
            Entry {
                tx,
                host,
                carrier: Arc::downgrade(&carrier),
                at: stand_in,
                forwarded_for: through.is_none(),
            },
        );
        forwarding.forwarder.on_admitted(link, stand_in);
    }

    /// The connection from `stand_in` ended without a link coming up on
    /// it, or after its link went: what was held for it is dropped.
    pub(crate) fn not_admitted(&self, stand_in: SocketAddr) {
        if let Some(forwarding) = self.forwarding.get()
            && !forwarding.links.is_sender(stand_in)
        {
            forwarding.forwarder.on_admission_failed(stand_in);
        }
    }

    /// Keeps `input` for a dump, as it is stepped on `core`.
    fn record(&self, core: &Relay, input: &Input) {
        lock(&self.history).window.record(core, input);
    }

    /// Writes one dump the core asked for, unless one like it was written
    /// this interval: the reason, the core's state when the window began,
    /// every input stepped since and the state now. The relay core sees
    /// ids, prefaces and tiers, never a token (tokens are checked before a
    /// link is admitted), so the file holds no secret.
    fn report(&self, reason: &DumpReason) {
        let now_ms = self.now_ms();
        let check = reason.check().map_or("", |check| check.id);
        let (admitted, window) = {
            let mut history = lock(&self.history);
            let Some(admitted) = history.dumps.reason(reason, now_ms) else {
                return;
            };
            (admitted, history.window.clone())
        };
        let reason = admitted.reason;
        let Some((dir, analytics)) = self.reports.get() else {
            tracing::error!(%reason, "the relay core asked for a dump, with nowhere to write it");
            return;
        };
        crate::reports::check_failed(
            analytics,
            crate::reports::Core::Relay,
            check,
            admitted.held_back,
        );
        let relay = lock(&self.core).clone();
        let text = window.render("the relay core", &reason, &relay);
        match window::write(dir, "relay", &text) {
            Ok(file) => tracing::error!(
                %reason,
                file = %file.display(),
                "the relay core asked for a dump"
            ),
            Err(error) => tracing::error!(
                %reason,
                %error,
                "the relay core asked for a dump, which could not be written"
            ),
        }
    }

    /// The hosts on `user`'s account holding a link here.
    pub(crate) fn neighbors(&self, user: relay_core::UserId) -> Vec<daemon_core::HostId> {
        lock(&self.core).neighbors(user)
    }

    fn now_ms(&self) -> u64 {
        match self.forwarding.get() {
            Some(forwarding) => forwarding.forwarder.now_ms(),
            None => u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The links the forwarder sends on, by link: each one's connection,
/// whose remote stand-in says where its packets go now, and its control
/// writer.
struct RelayLinks {
    /// Weak: the socket holds these links as its forward sink, and a
    /// profile that stops must let its socket go.
    socket: Weak<LinkSocket>,
    relaying: Weak<Relaying>,
    admitted: Mutex<Admitted>,
}

#[derive(Default)]
struct Admitted {
    links: HashMap<daemon_core::LinkId, Entry>,
    /// The link each stand-in's forwarded packets arrive on: a cache of
    /// where each link's connection was last seen.
    senders: HashMap<SocketAddr, daemon_core::LinkId>,
}

/// What the relay reads of a link it admitted: where quinn hears the far
/// end now (its stand-in on the socket, which moves when the far end's
/// network does), and whether the far end dialled it.
pub(crate) trait Seen: Send + Sync + 'static {
    fn remote_address(&self) -> Option<SocketAddr>;
    fn accepted(&self) -> bool;
}

impl Seen for super::QuicCarrier {
    fn remote_address(&self) -> Option<SocketAddr> {
        super::QuicCarrier::remote_address(self)
    }

    fn accepted(&self) -> bool {
        super::QuicCarrier::accepted(self)
    }
}

struct Entry {
    tx: LinkOutputTx,
    host: HostId,
    /// Weak: the connection holds its endpoint and so the socket, whose
    /// forward sink holds these links. The link registry holds it while
    /// the link lives.
    carrier: Weak<dyn Seen>,
    /// The stand-in the connection was last seen at.
    at: SocketAddr,
    /// The link is a host's this relay forwards for, not one to a
    /// forwarder this host sends through: the socket lets the host go
    /// with its last such link.
    forwarded_for: bool,
}

impl RelayLinks {
    /// The socket is told under the lock, so a link admitted while the
    /// host's last one is forgotten never leaves the host let go.
    fn admit(&self, link: daemon_core::LinkId, entry: Entry) {
        let mut admitted = lock(&self.admitted);
        if entry.forwarded_for
            && let Some(socket) = self.socket.upgrade()
        {
            socket.keep(entry.host);
        }
        admitted.senders.insert(entry.at, link);
        admitted.links.insert(link, entry);
    }

    /// The link is gone. The address its host was reached at stays the
    /// host's for a while: a link to it may still be on its way up there,
    /// and a host proving itself there later takes it. A host this relay
    /// forwards for is let go once its last link is gone and it falls
    /// silent.
    fn forget(&self, link: daemon_core::LinkId) {
        let socket = self.socket.upgrade();
        if let Some(socket) = &socket {
            socket.remove_forwarding(link);
        }
        let mut admitted = lock(&self.admitted);
        let Some(gone) = admitted.links.remove(&link) else {
            return;
        };
        admitted.senders.retain(|_, sender| *sender != link);
        if gone.forwarded_for
            && !admitted.links.values().any(|entry| entry.host == gone.host)
            && let Some(socket) = &socket
        {
            socket.let_go(gone.host);
        }
    }

    fn is_sender(&self, stand_in: SocketAddr) -> bool {
        lock(&self.admitted).senders.contains_key(&stand_in)
    }

    /// The link whose forwarded packets arrive from `stand_in`: one seen
    /// there, or one whose connection quinn has moved there since, after
    /// its sender's NAT rebound or its network changed. True in the second
    /// place: the link was found where it moved to.
    fn sender(&self, stand_in: SocketAddr) -> Option<(daemon_core::LinkId, bool)> {
        if let Some(link) = lock(&self.admitted).senders.get(&stand_in).copied() {
            return Some((link, false));
        }
        let moved = lock(&self.admitted)
            .links
            .iter()
            .map(|(link, entry)| (*link, entry.carrier.clone()))
            .collect::<Vec<_>>()
            .into_iter()
            .find(|(_, carrier)| {
                carrier
                    .upgrade()
                    .is_some_and(|carrier| carrier.remote_address() == Some(stand_in))
            })?
            .0;
        self.moved(moved, stand_in);
        Some((moved, true))
    }

    /// Where `link`'s packets go now: its connection's stand-in at this
    /// moment, which quinn moves when the far end's address changes.
    fn current(&self, link: daemon_core::LinkId) -> Option<SocketAddr> {
        let (carrier, at) = {
            let admitted = lock(&self.admitted);
            let entry = admitted.links.get(&link)?;
            (entry.carrier.clone(), entry.at)
        };
        let now = carrier.upgrade()?.remote_address()?;
        if now != at {
            self.moved(link, now);
            self.release_held(link, now);
        }
        Some(now)
    }

    /// `link`'s connection is at `stand_in` now: its host is known by the
    /// address behind it from here on, and packets from it arrive on the
    /// link.
    fn moved(&self, link: daemon_core::LinkId, stand_in: SocketAddr) {
        let mut admitted = lock(&self.admitted);
        let Some(entry) = admitted.links.get_mut(&link) else {
            return;
        };
        let accepted = entry
            .carrier
            .upgrade()
            .is_some_and(|carrier| carrier.accepted());
        let (old, host) = (entry.at, entry.host);
        entry.at = stand_in;
        if admitted.senders.get(&old) == Some(&link) {
            admitted.senders.remove(&old);
        }
        admitted.senders.insert(stand_in, link);
        drop(admitted);
        if accepted && let Some(socket) = self.socket.upgrade() {
            socket.name(stand_in, host);
        }
    }

    /// What was held from `stand_in` before `link` was found there goes
    /// through the table now.
    fn release_held(&self, link: daemon_core::LinkId, stand_in: SocketAddr) {
        if let Some(relaying) = self.relaying.upgrade()
            && let Some(forwarding) = relaying.forwarding.get()
        {
            forwarding.forwarder.on_admitted(link, stand_in);
        }
    }
}

impl ForwardLinks for RelayLinks {
    fn send(&self, link: daemon_core::LinkId, datagram: &[u8]) -> std::io::Result<()> {
        let stand_in = self.current(link).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "the link is gone")
        })?;
        self.socket
            .upgrade()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotConnected, "the socket is gone")
            })?
            .send_raw(stand_in, datagram)
    }

    fn refuse(&self, link: daemon_core::LinkId, dst: HostId, reason: ForwardRefusal) {
        let Some(tx) = lock(&self.admitted)
            .links
            .get(&link)
            .map(|entry| entry.tx.clone())
        else {
            return;
        };
        // A link too far behind to take its refusal hears it again when it
        // next sends there.
        let _ = tx.try_send(pb::Message {
            body: Some(pb::message::Body::ForwardRefused(pb::ForwardRefused {
                dst: dst.as_bytes().to_vec(),
                reason: reason as i32,
            })),
        });
    }

    fn dump(&self, reason: DumpReason) {
        if let Some(relaying) = self.relaying.upgrade() {
            relaying.report(&reason);
        }
    }

    fn record(&self, core: &Relay, input: &Input) {
        if let Some(relaying) = self.relaying.upgrade() {
            relaying.record(core, input);
        }
    }
}

/// The packets hosts send the relay's socket to forward: from an admitted
/// link's stand-in through the table, from anyone else into the hold.
struct Arrivals {
    forwarder: Arc<Forwarder>,
    links: Arc<RelayLinks>,
}

impl ForwardSink for Arrivals {
    fn arrived(&self, sender: SocketAddr, dst: HostId, payload: &[u8]) {
        match self.links.sender(sender) {
            Some((link, moved)) => {
                // Packets from the new address that came before quinn moved
                // the connection there were held; they go first.
                if moved {
                    self.forwarder.on_admitted(link, sender);
                }
                self.forwarder.on_packet(link, dst, payload)
            }
            None => self.forwarder.hold(sender, dst, payload),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::IoSliceMut;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use daemon_core::payload::CarrierKind;
    use quinn::AsyncUdpSocket;
    use quinn::udp::RecvMeta;
    use tokio::sync::mpsc;

    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::link::quic::{DEVICE_SERVER_NAME, LinkEndpoint, QuicCarrier};
    use crate::link::socket::{Datagrams, LinkId, Path, encode_delivered, encode_forwarded};
    use crate::trust::{SharedTrustStore, TrustStore};

    /// A short-header QUIC packet: the fixed bit set.
    const QUIC: &[u8] = &[0x40, 1, 2, 3, 4];

    /// An in-memory substrate: what was sent, and what to receive next.
    #[derive(Default)]
    struct Net {
        sent: Mutex<Vec<(SocketAddr, Vec<u8>)>>,
        incoming: Mutex<VecDeque<(SocketAddr, Vec<u8>)>>,
    }

    impl Net {
        fn take_sent(&self) -> Vec<(SocketAddr, Vec<u8>)> {
            std::mem::take(&mut lock(&self.sent))
        }
    }

    impl Datagrams for Net {
        fn send_to(&self, buf: &[u8], to: SocketAddr) -> std::io::Result<()> {
            lock(&self.sent).push((to, buf.to_vec()));
            Ok(())
        }

        fn poll_recv(
            &self,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<(usize, SocketAddr)>> {
            match lock(&self.incoming).pop_front() {
                Some((from, datagram)) => {
                    buf[..datagram.len()].copy_from_slice(&datagram);
                    Poll::Ready(Ok((datagram.len(), from)))
                }
                None => Poll::Pending,
            }
        }

        fn local_addr(&self) -> std::io::Result<SocketAddr> {
            Ok("198.51.100.1:443".parse().unwrap())
        }
    }

    /// One datagram from `from` reaches the socket: the stand-in quinn
    /// would hear it at, or nothing where the socket kept it (a packet to
    /// forward goes to the forwarder).
    fn arrive(
        net: &Net,
        socket: &LinkSocket,
        from: SocketAddr,
        datagram: &[u8],
    ) -> Option<SocketAddr> {
        lock(&net.incoming).push_back((from, datagram.to_vec()));
        let mut storage = [0u8; 2048];
        let mut bufs = [IoSliceMut::new(&mut storage)];
        let mut meta = [RecvMeta::default()];
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match socket.poll_recv(&mut cx, &mut bufs, &mut meta) {
            Poll::Ready(Ok(1)) => Some(meta[0].addr),
            Poll::Pending => None,
            other => panic!("unexpected receive: {other:?}"),
        }
    }

    /// A connection the far end dialled, at whichever stand-in quinn has
    /// it at.
    struct Dialled(Mutex<SocketAddr>);

    impl Dialled {
        fn at(stand_in: SocketAddr) -> Arc<Self> {
            Arc::new(Self(Mutex::new(stand_in)))
        }

        /// quinn moved the connection: its packets came from elsewhere.
        fn moved_to(&self, stand_in: SocketAddr) {
            *lock(&self.0) = stand_in;
        }
    }

    impl Seen for Dialled {
        fn remote_address(&self) -> Option<SocketAddr> {
            Some(*lock(&self.0))
        }

        fn accepted(&self) -> bool {
            true
        }
    }

    fn core(host: HostId) -> daemon_core::HostId {
        daemon_core::HostId(host.into_bytes())
    }

    /// Two hosts of one paid account hold links to a relay.
    fn link_up(relaying: &Relaying, link: u64, host: HostId, user: relay_core::UserId) {
        relaying.step([Input::LinkUp {
            link: daemon_core::LinkId(link),
            user,
            host: core(host),
            tier: Some(relay_core::Tier::Pro),
            instance: [0; 16],
            direct: None,
            advertised: Vec::new(),
        }]);
    }

    /// A relay with one host that stays, `stayer`, linked at `stayer_lan`
    /// over `link`.
    struct Stayed {
        net: Arc<Net>,
        socket: Arc<LinkSocket>,
        relaying: Arc<Relaying>,
        user: relay_core::UserId,
        stayer: HostId,
        stayer_lan: SocketAddr,
        link: Arc<Dialled>,
    }

    fn relay_with_a_stayer() -> Stayed {
        let relay = HostId::new_v4();
        let net = Arc::new(Net::default());
        let socket = LinkSocket::bind(net.clone(), relay).unwrap();
        let relaying = Relaying::new(relay);
        relaying.forward_on(socket.clone());
        let user = relay_core::UserId([7; 16]);
        let stayer = HostId::new_v4();
        let stayer_lan: SocketAddr = "203.0.113.1:6000".parse().unwrap();
        let link = Dialled::at(arrive(&net, &socket, stayer_lan, QUIC).unwrap());
        link_up(&relaying, 1000, stayer, user);
        let (tx, _rx) = mpsc::channel(8);
        relaying.admitted(daemon_core::LinkId(1000), tx, stayer, link.clone(), None);
        Stayed {
            net,
            socket,
            relaying,
            user,
            stayer,
            stayer_lan,
            link,
        }
    }

    /// `secs` pass with the stayer sending every ten seconds: arrivals are
    /// when the socket sweeps.
    async fn pass(net: &Net, socket: &LinkSocket, stayer_lan: SocketAddr, secs: u64) {
        for _ in 0..secs / 10 {
            tokio::time::advance(Duration::from_secs(10)).await;
            arrive(net, socket, stayer_lan, QUIC).unwrap();
        }
    }

    /// A relay holds nothing for hosts that have gone: a hundred hosts
    /// link, each rebinds twice (quinn moving its connection, the host
    /// named at each new stand-in), and their links go. Two minutes on,
    /// the relay's socket holds what it held before them, and the host
    /// that stayed is still routed.
    #[tokio::test(start_paused = true)]
    async fn relay_socket_lets_go_of_hosts_that_rebound_and_left() {
        let Stayed {
            net,
            socket,
            relaying,
            user,
            stayer,
            stayer_lan,
            link: stayer_link,
        } = relay_with_a_stayer();
        let baseline = socket.held();

        for n in 0..100u64 {
            let host = HostId::new_v4();
            let link = n + 1;
            let first = SocketAddr::new([192, 0, 2, 1].into(), 10_000 + n as u16);
            let carrier = Dialled::at(arrive(&net, &socket, first, QUIC).unwrap());
            link_up(&relaying, link, host, user);
            let (tx, _rx) = mpsc::channel(8);
            relaying.admitted(daemon_core::LinkId(link), tx, host, carrier.clone(), None);
            for rebind in 1..=2u8 {
                let next = SocketAddr::new([192, 0, 2, 1 + rebind].into(), 10_000 + n as u16);
                carrier.moved_to(arrive(&net, &socket, next, QUIC).unwrap());
                arrive(&net, &socket, next, &encode_forwarded(stayer, b"\x40hi"));
                assert_eq!(socket.address_of(*lock(&carrier.0)), Some(next));
            }
            relaying.step([Input::LinkDown {
                link: daemon_core::LinkId(link),
            }]);
        }
        net.take_sent();
        let (peers, stand_ins, _, _) = socket.held();
        assert!(peers >= baseline.0 + 100 && stand_ins >= baseline.1 + 300);

        pass(&net, &socket, stayer_lan, 120).await;
        assert_eq!(socket.held(), baseline, "nothing of the gone hosts stays");
        let stayer_at = *lock(&stayer_link.0);
        assert_eq!(socket.peer_of(stayer_at), Some(stayer));
        assert_eq!(
            socket.address_of(stayer_at),
            Some(stayer_lan),
            "the host that stayed is still routed"
        );
    }

    /// A host whose link goes and that links again within half a minute
    /// is kept at the stand-in it had, and reached there: as is one whose
    /// new link is admitted before its old one is forgotten.
    #[tokio::test(start_paused = true)]
    async fn relay_socket_keeps_a_host_that_returns() {
        let Stayed {
            net,
            socket,
            relaying,
            user,
            stayer,
            stayer_lan,
            link: _stayer_link,
        } = relay_with_a_stayer();
        let (desk, phone) = (HostId::new_v4(), HostId::new_v4());
        let desk_lan: SocketAddr = "203.0.113.10:6000".parse().unwrap();
        let phone_lan: SocketAddr = "203.0.113.20:6000".parse().unwrap();
        let desk_at = arrive(&net, &socket, desk_lan, QUIC).unwrap();
        let phone_at = arrive(&net, &socket, phone_lan, QUIC).unwrap();
        let mut carriers = Vec::new();
        for (link, host, at) in [(1, desk, desk_at), (2, phone, phone_at)] {
            link_up(&relaying, link, host, user);
            let (tx, _rx) = mpsc::channel(8);
            carriers.push(Dialled::at(at));
            relaying.admitted(
                daemon_core::LinkId(link),
                tx,
                host,
                carriers[carriers.len() - 1].clone(),
                None,
            );
        }

        // The desk's link goes; it is back half a minute later, from the
        // same address, which still leads to its stand-in.
        relaying.step([Input::LinkDown {
            link: daemon_core::LinkId(1),
        }]);
        pass(&net, &socket, stayer_lan, 30).await;
        assert_eq!(arrive(&net, &socket, desk_lan, QUIC), Some(desk_at));
        link_up(&relaying, 3, desk, user);
        let (tx, _rx) = mpsc::channel(8);
        carriers.push(Dialled::at(desk_at));
        relaying.admitted(daemon_core::LinkId(3), tx, desk, carriers[2].clone(), None);

        // The phone's second link is admitted before the core is told of
        // it, and so before its first link is displaced.
        let (tx, _rx) = mpsc::channel(8);
        carriers.push(Dialled::at(phone_at));
        relaying.admitted(daemon_core::LinkId(4), tx, phone, carriers[3].clone(), None);
        link_up(&relaying, 4, phone, user);
        relaying.step([Input::LinkDown {
            link: daemon_core::LinkId(2),
        }]);

        pass(&net, &socket, stayer_lan, 120).await;
        net.take_sent();
        for (host, at, lan) in [(desk, desk_at, desk_lan), (phone, phone_at, phone_lan)] {
            assert_eq!(socket.peer_of(at), Some(host), "kept at its stand-in");
            arrive(
                &net,
                &socket,
                stayer_lan,
                &encode_forwarded(host, b"\x40yo"),
            );
            assert_eq!(
                net.take_sent(),
                vec![(lan, encode_delivered(stayer, b"\x40yo"))],
                "and reached there"
            );
        }
    }

    /// The relay keys a sender by its link, not the address it was first
    /// seen at: the desk's network changes while it forwards to the phone,
    /// and what it sent from the new address before quinn moved its
    /// connection there was held, then goes on as soon as the link is
    /// found there; the phone moving is followed at the next send; and a
    /// host turning up at the desk's address once its link is gone is
    /// named there and reached there.
    #[test]
    fn relaying_follows_a_sender_whose_connection_moved() {
        let relay = HostId::new_v4();
        let net = Arc::new(Net::default());
        let socket = LinkSocket::bind(net.clone(), relay).unwrap();
        let relaying = Relaying::new(relay);
        relaying.forward_on(socket.clone());
        let user = relay_core::UserId([7; 16]);
        let (desk, phone) = (HostId::new_v4(), HostId::new_v4());
        let (desk_lan, phone_lan): (SocketAddr, SocketAddr) = (
            "203.0.113.10:6000".parse().unwrap(),
            "203.0.113.20:6000".parse().unwrap(),
        );
        let desk_at = arrive(&net, &socket, desk_lan, QUIC).unwrap();
        let phone_at = arrive(&net, &socket, phone_lan, QUIC).unwrap();
        let (desk_link, phone_link) = (Dialled::at(desk_at), Dialled::at(phone_at));
        for (link, host, carrier) in [(1, desk, desk_link.clone()), (2, phone, phone_link.clone())]
        {
            link_up(&relaying, link, host, user);
            let (tx, _rx) = mpsc::channel(8);
            relaying.admitted(daemon_core::LinkId(link), tx, host, carrier, None);
        }
        assert_eq!(
            socket.peer_of(desk_at),
            Some(desk),
            "the admission named it"
        );
        assert_eq!(socket.peer_of(phone_at), Some(phone));

        assert_eq!(
            arrive(
                &net,
                &socket,
                desk_lan,
                &encode_forwarded(phone, b"\x40one")
            ),
            None
        );
        assert_eq!(
            net.take_sent(),
            vec![(phone_lan, encode_delivered(desk, b"\x40one"))]
        );

        // The desk's network changes. Its next packet to forward comes
        // before quinn has moved its connection: held.
        let desk_cell: SocketAddr = "192.0.2.99:41000".parse().unwrap();
        assert_eq!(
            arrive(
                &net,
                &socket,
                desk_cell,
                &encode_forwarded(phone, b"\x40two")
            ),
            None
        );
        assert!(net.take_sent().is_empty(), "held until the sender is known");
        let moved = arrive(&net, &socket, desk_cell, QUIC).expect("for quinn");
        desk_link.moved_to(moved);
        assert_eq!(
            arrive(
                &net,
                &socket,
                desk_cell,
                &encode_forwarded(phone, b"\x40three")
            ),
            None
        );
        assert_eq!(
            net.take_sent(),
            vec![
                (phone_lan, encode_delivered(desk, b"\x40two")),
                (phone_lan, encode_delivered(desk, b"\x40three")),
            ],
            "what was held went first"
        );
        assert_eq!(socket.peer_of(moved), Some(desk));
        assert_eq!(socket.address_of(desk_at), Some(desk_cell));
        assert_ne!(
            socket.stand_in_at(desk_lan),
            desk_at,
            "the old address is let go"
        );

        // The phone moves: the next packet for it goes where quinn has it.
        let phone_cell: SocketAddr = "192.0.2.77:52000".parse().unwrap();
        phone_link.moved_to(arrive(&net, &socket, phone_cell, QUIC).unwrap());
        assert_eq!(
            arrive(
                &net,
                &socket,
                desk_cell,
                &encode_forwarded(phone, b"\x40four")
            ),
            None
        );
        assert_eq!(
            net.take_sent(),
            vec![(phone_cell, encode_delivered(desk, b"\x40four"))]
        );
        assert_eq!(socket.address_of(phone_at), Some(phone_cell));

        // The desk's link goes and a tablet turns up at its address: named
        // there once admitted, it is what is forwarded there reaches.
        relaying.step([Input::LinkDown {
            link: daemon_core::LinkId(1),
        }]);
        let tablet = HostId::new_v4();
        let tablet_at = arrive(&net, &socket, desk_cell, QUIC).unwrap();
        link_up(&relaying, 3, tablet, user);
        let (tx, _rx) = mpsc::channel(8);
        let tablet_link = Dialled::at(tablet_at);
        relaying.admitted(
            daemon_core::LinkId(3),
            tx,
            tablet,
            tablet_link.clone(),
            None,
        );
        assert_eq!(socket.peer_of(tablet_at), Some(tablet));
        assert_eq!(
            arrive(
                &net,
                &socket,
                phone_cell,
                &encode_forwarded(tablet, b"\x40five")
            ),
            None
        );
        assert_eq!(
            net.take_sent(),
            vec![(desk_cell, encode_delivered(phone, b"\x40five"))]
        );
    }

    fn trusting(peers: &[&DeviceIdentity]) -> SharedTrustStore {
        let mut trust = TrustStore::default();
        for peer in peers {
            trust
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "peer".to_string(),
                    None,
                    chrono::Utc::now(),
                )
                .unwrap();
        }
        Arc::new(std::sync::RwLock::new(trust))
    }

    fn loopback_endpoint(own: HostId, server: Option<quinn::ServerConfig>) -> LinkEndpoint {
        LinkEndpoint::new(
            LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), None).unwrap(),
            own,
            server,
        )
        .unwrap()
    }

    /// Probes `to` through the relay until an answer comes back, or the
    /// time runs out.
    async fn answered(
        from: &LinkEndpoint,
        answers: &mut mpsc::UnboundedReceiver<crate::link::socket::ProbeAnswered>,
        to: HostId,
        within: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        let mut nonce = 0;
        while tokio::time::Instant::now() < deadline {
            nonce += 1;
            // A full socket buffer loses the probe, as the network would.
            let _ = from.socket.probe(to, Path::Forwarded(LinkId(1)), nonce);
            if let Ok(Some(answer)) =
                tokio::time::timeout(Duration::from_millis(50), answers.recv()).await
                && answer.from == to
            {
                return true;
            }
        }
        false
    }

    /// Over real QUIC on loopback: each host's network changes in turn
    /// while it and the other probe each other through the relay, and the
    /// probes are answered again well inside the hold's second, with no
    /// link closing.
    #[tokio::test(flavor = "multi_thread")]
    async fn relaying_carries_forwarded_traffic_across_a_sender_rebinding() {
        let relay = DeviceIdentity::for_test(HostId::new_v4());
        let hosts = [
            DeviceIdentity::for_test(HostId::new_v4()),
            DeviceIdentity::for_test(HostId::new_v4()),
        ];
        let relay_endpoint = loopback_endpoint(
            relay.host_id,
            Some(
                relay
                    .quic_server_config(
                        trusting(&[&hosts[0], &hosts[1]]),
                        &crate::link::resumption::SessionTickets::in_memory(16),
                    )
                    .unwrap(),
            ),
        );
        let relay_addr = relay_endpoint.local_addr().unwrap();
        let relaying = Relaying::new(relay.host_id);
        relaying.forward_on(relay_endpoint.socket.clone());
        let user = relay_core::UserId([9; 16]);
        let mut ends = Vec::new();
        let mut carriers = Vec::new();
        for (index, host) in hosts.iter().enumerate() {
            let endpoint = loopback_endpoint(host.host_id, None);
            let config = host
                .quic_client_config_for_peer(trusting(&[&relay]), relay.host_id)
                .unwrap();
            let (dialled, accepted) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(
                    async {
                        endpoint
                            .connect_with(config, relay_addr, DEVICE_SERVER_NAME)
                            .unwrap()
                            .await
                            .unwrap()
                    },
                    async {
                        relay_endpoint
                            .endpoint
                            .accept()
                            .await
                            .unwrap()
                            .await
                            .unwrap()
                    }
                )
            })
            .await
            .expect("handshake completes");
            let carrier: Arc<dyn Seen> = Arc::new(QuicCarrier::from_accepted_with_kind(
                accepted,
                CarrierKind::Relay,
            ));
            let link = index as u64 + 1;
            link_up(&relaying, link, host.host_id, user);
            let (tx, rx) = mpsc::channel(8);
            relaying.admitted(
                daemon_core::LinkId(link),
                tx,
                host.host_id,
                carrier.clone(),
                None,
            );
            let via = endpoint.socket.stand_in_at(relay_addr);
            endpoint
                .socket
                .add_forwarding(LinkId(1), via, PathKind::RelayUdp);
            let answers = endpoint.socket.probe_answers();
            carriers.push((carrier, rx));
            ends.push((endpoint, answers, dialled));
        }
        let ids = [hosts[0].host_id, hosts[1].host_id];
        for (from, to) in [(0, 1), (1, 0)] {
            let (endpoint, answers, _) = &mut ends[from];
            assert!(answered(endpoint, answers, ids[to], Duration::from_secs(5)).await);
        }

        for moving in [0, 1] {
            let substrate =
                LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), None).unwrap();
            ends[moving].0.rebind(substrate).unwrap();
            for (from, to) in [(moving, 1 - moving), (1 - moving, moving)] {
                let (endpoint, answers, _) = &mut ends[from];
                assert!(
                    answered(endpoint, answers, ids[to], Duration::from_millis(900)).await,
                    "host {from} reaches host {to} after host {moving} moved"
                );
            }
        }
        for (_, _, dialled) in &ends {
            assert_eq!(dialled.close_reason(), None);
        }
    }

    /// A dump the relay core asks for leaves a file in the reports
    /// directory: the reason, the core's state when the window began,
    /// every input stepped since and the state now. A relay whose budget is
    /// zero asks at its first step measured over it.
    #[test]
    fn a_dump_the_relay_core_asks_for_is_a_file_of_its_state_and_inputs() {
        let reports = tempfile::tempdir().unwrap();
        let own = HostId::new_v4();
        let relaying = Relaying::with_relay(
            own,
            Relay::with_budget(
                daemon_core::HostId(own.into_bytes()),
                daemon_core::StepBudget { micros: 0 },
            ),
        );
        relaying.report_to(reports.path().to_owned(), analytics::Analytics::off());
        let written = || {
            std::fs::read_dir(reports.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        };
        let mut now_ms = 0;
        // Any step takes longer than nothing, given a clock fine enough.
        while written().is_empty() {
            now_ms += 1;
            relaying.step([Input::Tick { now_ms }]);
        }
        let [file] = &written()[..] else {
            panic!("one file: {:?}", written());
        };
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("relay-") && name.ends_with(".txt"),
            "{name}"
        );
        let text = std::fs::read_to_string(file).unwrap();
        println!("{}\n{text}", file.display());
        let header = format!("the relay core, amux {}\n", crate::version());
        assert!(text.starts_with(&header), "{text}");
        assert!(
            text.contains(&format!("reason: {RELAY_CORE_DUMP}"))
                && text.contains("over its budget of 0 µs"),
            "{text}"
        );
        assert!(
            text.contains("state at the window's start:\nRelay {"),
            "{text}"
        );
        // Steps quicker than the clock's microsecond pass the budget, so
        // the window may have moved on from the first tick: it holds an
        // unbroken run of ticks ending with the one that asked.
        let held: usize = text
            .split_once("inputs since (")
            .and_then(|(_, rest)| rest.split_once("):"))
            .and_then(|(count, _)| count.parse().ok())
            .expect("the dump counts its inputs");
        let first = now_ms - held as u64 + 1;
        assert!(
            text.contains(&format!("\n1 Tick {{ now_ms: {first} }}\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!("\n{held} Tick {{ now_ms: {now_ms} }}\n")),
            "{text}"
        );
        assert!(text.contains("state now:\nRelay {"), "{text}");
        assert!(text.contains(&format!("now_ms: {now_ms},")), "{text}");
    }

    #[test]
    fn a_violation_the_relay_core_repeats_is_reported_once_per_interval() {
        let broke = DumpReason::Violation(daemon_core::Violation {
            invariant: relay_core::NOTHING_BEFORE_ADMISSION,
            detail: "link 9 is down".into(),
        });
        let mut dumps = Dumps::default();
        let first = dumps.reason(&broke, 1_000).unwrap().reason;
        assert!(first.starts_with(RELAY_CORE_DUMP), "{first}");
        for at in [1_001, 2_000, 1_000 + DumpLimit::INTERVAL_MS - 1] {
            assert_eq!(dumps.reason(&broke, at), None);
        }
        let second = dumps
            .reason(&broke, 1_000 + DumpLimit::INTERVAL_MS)
            .unwrap();
        assert_eq!(second.held_back, 3);
        let second = second.reason;
        assert!(
            second.ends_with("(3 more like it were not dumped since the last)"),
            "{second}"
        );
    }
}
