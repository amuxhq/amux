//! The path machines under the link socket.
//!
//! Every peer this profile links to or remembers has a [`Paths`] machine of
//! its own, which decides which path its link's packets take: straight to
//! one of its addresses, or through a forwarding link. [`PathDriver`]
//! steps each one with what happens to it (links coming up and going,
//! addresses listed, probes answered, packets arriving, forwarding links
//! coming and going, refusals, the plan, the network changing, the clock)
//! and carries out what it asks: probes from the one socket, and the path
//! the socket puts the peer's packets on. quinn sends to the peer's
//! stand-in throughout, so a path change is invisible to the connection and
//! every stream open on it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use daemon_core::payload::{CarrierKind, PeerAddress, Source};
use path_core::{ForwardRefusal, Input, LinkId, Output, PROBE_TIMEOUT_MS, Path, Paths};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::carrier::LinkCarrier;
use super::link_log::{EarlyData, LinkEvent, LinkTransition, PathKind};
use super::socket::{LinkSocket, PathEvent, ProbeAnswered};
use crate::HostId;
use crate::driver::CoreLinks;

/// The path machines of one profile's socket.
pub struct PathDriver {
    socket: Arc<LinkSocket>,
    /// Where a link moving to a path of another kind is reported, so the
    /// host reads by it.
    core: Option<CoreLinks>,
    state: Mutex<State>,
    /// Woken when a machine asks for an earlier tick.
    wake: Arc<Notify>,
    epoch: Instant,
}

struct State {
    machines: HashMap<HostId, Machine>,
    paid: bool,
    /// The host's interface addresses, as last told.
    local: Vec<IpAddr>,
    /// The forwarding links to the relay: a path to every peer.
    relays: BTreeSet<LinkId>,
    next_nonce: u64,
}

/// One peer's machine and what the driver keeps beside it.
struct Machine {
    paths: Paths,
    /// The peer's links up on this socket.
    links: BTreeSet<LinkId>,
    /// The addresses the machine was given, and where each came from.
    candidates: BTreeMap<SocketAddr, Source>,
    /// The probe outstanding on each path: only its nonce's answer counts.
    probes: HashMap<Path, Probe>,
    /// The tick the machine last asked for.
    armed: Option<u64>,
    /// The kind of path last carrying the link, as the link log names it.
    kind: Option<PathKind>,
}

#[derive(Clone, Copy)]
struct Probe {
    nonce: u64,
    sent: Instant,
}

impl PathDriver {
    /// Drives a machine for every peer of `socket`'s profile, reporting
    /// path changes to `core`, until the returned task is aborted or the
    /// driver is dropped.
    pub(crate) fn start(
        socket: Arc<LinkSocket>,
        core: Option<CoreLinks>,
    ) -> (Arc<Self>, JoinHandle<()>) {
        let events = socket.path_events();
        let answers = socket.probe_answers();
        let driver = Arc::new(Self {
            socket,
            core,
            state: Mutex::new(State {
                machines: HashMap::new(),
                paid: false,
                local: Vec::new(),
                relays: BTreeSet::new(),
                next_nonce: uuid::Uuid::new_v4().as_u64_pair().0,
            }),
            wake: Arc::new(Notify::new()),
            epoch: Instant::now(),
        });
        let task = tokio::spawn(run(
            Arc::downgrade(&driver),
            driver.wake.clone(),
            events,
            answers,
        ));
        (driver, task)
    }

    /// A link to `peer` came up on `carrier`. One with no stand-in on this
    /// socket (a byte stream's endpoint of its own) has no paths to choose.
    pub(crate) fn link_up(&self, link: LinkId, peer: HostId, carrier: &dyn LinkCarrier) {
        if let Some(stand_in) = carrier.remote_address() {
            self.link_up_at(link, peer, stand_in, carrier.path_kind());
        }
    }

    /// A link to `peer` came up on its connection at `stand_in`, on a path
    /// of `kind`.
    fn link_up_at(&self, link: LinkId, peer: HostId, stand_in: SocketAddr, kind: PathKind) {
        if self.socket.peer_of(stand_in) != Some(peer) {
            // A link this host dialled by address, a pairing's or a
            // discovered host's, stays at the stand-in of whoever answered
            // there, so no path of the peer's carries it. The peer still
            // has its machine, which a network change tells.
            let now_ms = self.now_ms();
            self.lock().machine(peer, now_ms);
            return;
        }
        let path = self.socket.path_of(stand_in);
        let mut state = self.lock();
        let now_ms = self.now_ms();
        let machine = state.machine(peer, now_ms);
        let first = machine.links.is_empty();
        machine.links.insert(link);
        if !first {
            return;
        }
        machine.kind = Some(kind);
        let mut inputs = Vec::new();
        // A link through a paired daemon is carried on that daemon's
        // forwarding link, a path to this peer alone.
        if let Some(Path::Forwarded(through)) = path {
            inputs.push(Input::ForwardingUp { link: through });
        }
        inputs.push(Input::LinkUp);
        if let Some(path) = path {
            inputs.push(Input::PacketArrived { path });
        }
        self.step(&mut state, peer, inputs);
    }

    /// The link went. Its peer's machine stays, remembering its paths for
    /// the next link.
    pub(crate) fn link_down(&self, link: LinkId) {
        let mut state = self.lock();
        let Some((peer, machine)) = state
            .machines
            .iter_mut()
            .find(|(_, machine)| machine.links.contains(&link))
        else {
            return;
        };
        let peer = *peer;
        machine.links.remove(&link);
        if machine.links.is_empty() {
            machine.kind = None;
            self.step(&mut state, peer, vec![Input::LinkDown]);
        }
    }

    /// What `peer` lists for itself now, each address with where it came
    /// from. Addresses no longer listed are gone.
    pub(crate) fn addresses(&self, peer: HostId, addrs: &[PeerAddress]) {
        if peer == self.socket.own() {
            return;
        }
        let mut state = self.lock();
        let now_ms = self.now_ms();
        let machine = state.machine(peer, now_ms);
        let fresh: BTreeMap<SocketAddr, Source> =
            addrs.iter().map(|addr| (addr.addr, addr.source)).collect();
        let mut inputs: Vec<Input> = machine
            .candidates
            .keys()
            .filter(|addr| !fresh.contains_key(addr))
            .map(|addr| Input::CandidateGone { addr: *addr })
            .collect();
        inputs.extend(
            fresh
                .iter()
                .filter(|(addr, source)| machine.candidates.get(addr) != Some(source))
                .map(|(addr, source)| Input::Candidate {
                    addr: *addr,
                    source: *source,
                }),
        );
        machine.candidates = fresh;
        self.step(&mut state, peer, inputs);
    }

    /// The forwarding link `through` refused to carry packets to `peer`.
    pub(crate) fn forward_refused(&self, peer: HostId, through: LinkId, reason: ForwardRefusal) {
        let mut state = self.lock();
        if !state.machines.contains_key(&peer) {
            return;
        }
        self.step(
            &mut state,
            peer,
            vec![Input::ForwardRefused {
                link: through,
                reason,
            }],
        );
    }

    /// The account's plan, as the relay last said it.
    pub(crate) fn plan(&self, paid: bool) {
        let mut state = self.lock();
        if state.paid == paid {
            return;
        }
        state.paid = paid;
        self.step_all(&mut state, || Input::Plan { paid });
    }

    /// The host's network changed: every path is suspect, and each machine
    /// probes its paths again. How many machines were told.
    pub(crate) fn network_changed(&self, addrs: Vec<IpAddr>) -> usize {
        let mut state = self.lock();
        state.local = addrs.clone();
        // An answer to a probe sent before the change says nothing of the
        // network now.
        for machine in state.machines.values_mut() {
            machine.probes.clear();
        }
        self.step_all(&mut state, || Input::NetworkChanged {
            addrs: addrs.clone(),
        })
    }

    /// The path carrying the link to `peer`, as its machine chose it.
    pub fn carrying(&self, peer: HostId) -> Option<Path> {
        self.lock()
            .machines
            .get(&peer)
            .and_then(|machine| machine.paths.carrying().copied())
    }

    /// How many peers have a machine.
    pub fn machines(&self) -> usize {
        self.lock().machines.len()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn event(&self, event: PathEvent) {
        let mut state = self.lock();
        match event {
            PathEvent::ForwardingUp { link, kind } => {
                if !matches!(kind, PathKind::RelayUdp | PathKind::RelayTcp) {
                    return;
                }
                state.relays.insert(link);
                self.step_all(&mut state, || Input::ForwardingUp { link });
            }
            PathEvent::ForwardingDown { link } => {
                state.relays.remove(&link);
                self.step_all(&mut state, || Input::ForwardingDown { link });
            }
            PathEvent::Arrived { peer, path } => {
                if state
                    .machines
                    .get(&peer)
                    .is_some_and(|machine| !machine.links.is_empty())
                {
                    self.step(&mut state, peer, vec![Input::PacketArrived { path }]);
                }
            }
        }
    }

    /// A probe's answer. Only the answer to the probe outstanding on a path,
    /// within its timeout, says the path works: a late or repeated one
    /// matches nothing.
    fn answered(&self, answer: ProbeAnswered) {
        let mut state = self.lock();
        let Some(machine) = state.machines.get_mut(&answer.from) else {
            return;
        };
        let Some(path) = machine
            .probes
            .iter()
            .find(|(_, probe)| probe.nonce == answer.nonce)
            .map(|(path, _)| *path)
        else {
            return;
        };
        let probe = machine.probes.remove(&path).expect("just found");
        let rtt = probe.sent.elapsed();
        if rtt.as_millis() >= u128::from(PROBE_TIMEOUT_MS) {
            return;
        }
        let rtt_ms = u32::try_from(rtt.as_millis()).unwrap_or(u32::MAX);
        self.step(
            &mut state,
            answer.from,
            vec![Input::ProbeAnswered { path, rtt_ms }],
        );
    }

    /// Ticks every machine whose time has come; when the next one is due.
    fn tick(&self) -> Option<Instant> {
        let mut state = self.lock();
        let now_ms = self.now_ms();
        let due: Vec<HostId> = state
            .machines
            .iter()
            .filter(|(_, machine)| machine.armed.is_some_and(|at| at <= now_ms))
            .map(|(peer, _)| *peer)
            .collect();
        for peer in due {
            if let Some(machine) = state.machines.get_mut(&peer) {
                machine.armed = None;
            }
            self.step(&mut state, peer, vec![Input::Tick { now_ms }]);
        }
        state
            .machines
            .values()
            .filter_map(|machine| machine.armed)
            .min()
            .map(|at_ms| self.epoch + std::time::Duration::from_millis(at_ms))
    }

    fn step_all(&self, state: &mut State, input: impl Fn() -> Input) -> usize {
        let peers: Vec<HostId> = state.machines.keys().copied().collect();
        for peer in &peers {
            self.step(state, *peer, vec![input()]);
        }
        peers.len()
    }

    /// Steps `peer`'s machine with each input in turn, and with whatever
    /// carrying out its outputs feeds back, until nothing is left.
    fn step(&self, state: &mut State, peer: HostId, inputs: Vec<Input>) {
        let mut queue = std::collections::VecDeque::from(inputs);
        let now_ms = self.now_ms();
        let mut out = Vec::new();
        while let Some(input) = queue.pop_front() {
            let next_nonce = &mut state.next_nonce;
            let Some(machine) = state.machines.get_mut(&peer) else {
                return;
            };
            machine.paths.step(Input::Tick { now_ms }, &mut out);
            machine.paths.step(input, &mut out);
            for output in out.drain(..) {
                match output {
                    Output::Probe { path } => {
                        *next_nonce = next_nonce.wrapping_add(1);
                        let nonce = *next_nonce;
                        machine.probes.insert(
                            path,
                            Probe {
                                nonce,
                                sent: Instant::now(),
                            },
                        );
                        // A probe that cannot go is a path that did not
                        // answer.
                        if self.socket.probe(peer, path, nonce).is_err() {
                            machine.probes.remove(&path);
                            queue.push_back(Input::ProbeTimedOut { path });
                        }
                    }
                    Output::Carry { path } => self.carry(peer, machine, path),
                    Output::SendOnAll { paths } => {
                        // Until a link is up its first flight chose where
                        // its packets go, and while one path still carries
                        // it the link stays there until the machine names
                        // the next.
                        if !machine.links.is_empty() && self.socket.carrying(peer).is_none() {
                            self.socket.send_on_all(peer, paths, false);
                        }
                    }
                    Output::DropCandidate { addr } => {
                        machine.candidates.remove(&addr);
                        machine.probes.remove(&Path::Direct(addr));
                        machine.probes.remove(&Path::Punched(addr));
                    }
                    Output::Arm { at_ms } => {
                        machine.armed = Some(at_ms);
                        self.wake.notify_one();
                    }
                    Output::Dump { reason } => {
                        tracing::error!(%peer, %reason, "a path machine failed a check");
                    }
                }
            }
        }
    }

    /// Puts `peer`'s packets on `path`. quinn keeps sending to the peer's
    /// stand-in; only the socket's route moves.
    fn carry(&self, peer: HostId, machine: &mut Machine, path: Path) {
        if machine.links.is_empty() {
            return;
        }
        if self.socket.carrying(peer) != Some(path) {
            self.socket.set_carrying(peer, path);
        }
        let Some(kind) = self.socket.kind_of(path) else {
            return;
        };
        if machine.kind == Some(kind) {
            return;
        }
        machine.kind = Some(kind);
        LinkEvent {
            peer,
            event: LinkTransition::PathChanged,
            path: kind,
            resumed: false,
            early_data: EarlyData::None,
            first_packet_to_catch_up_ms: None,
        }
        .write(self.socket.own());
        if let Some(core) = &self.core {
            for link in &machine.links {
                core.report(daemon_core::Input::LinkCarried {
                    link: *link,
                    kind: carrier_kind(kind),
                });
            }
        }
    }
}

impl State {
    /// `peer`'s machine, made on the plan and network as they are now,
    /// with every relay path, if it has none.
    fn machine(&mut self, peer: HostId, now_ms: u64) -> &mut Machine {
        let (paid, local, relays) = (self.paid, self.local.clone(), self.relays.clone());
        self.machines.entry(peer).or_insert_with(|| {
            let mut paths = Paths::new(now_ms, paid);
            let mut out = Vec::new();
            if !local.is_empty() {
                paths.step(Input::NetworkChanged { addrs: local }, &mut out);
            }
            for link in relays {
                paths.step(Input::ForwardingUp { link }, &mut out);
            }
            // Nothing is up yet: nothing to probe, carry or send on.
            let armed = out.iter().rev().find_map(|output| match output {
                Output::Arm { at_ms } => Some(*at_ms),
                _ => None,
            });
            Machine {
                paths,
                links: BTreeSet::new(),
                candidates: BTreeMap::new(),
                probes: HashMap::new(),
                armed,
                kind: None,
            }
        })
    }
}

/// The kind of carrier the core reads a link by, from the path carrying it.
fn carrier_kind(kind: PathKind) -> CarrierKind {
    match kind {
        PathKind::Lan | PathKind::Direct | PathKind::Punched => CarrierKind::Direct,
        PathKind::RelayUdp | PathKind::RelayTcp | PathKind::ViaDaemon => CarrierKind::Forwarded,
        PathKind::Ssh => CarrierKind::Ssh,
    }
}

/// The refusal as a path machine reads it.
pub(crate) fn refusal(reason: wire::pb::ForwardRefusal) -> ForwardRefusal {
    match reason {
        wire::pb::ForwardRefusal::PaymentRequired => ForwardRefusal::PaymentRequired,
        wire::pb::ForwardRefusal::RateLimited => ForwardRefusal::RateLimited,
        wire::pb::ForwardRefusal::ShuttingDown => ForwardRefusal::ShuttingDown,
        _ => ForwardRefusal::NoRoute,
    }
}

async fn run(
    driver: Weak<PathDriver>,
    wake: Arc<Notify>,
    mut events: mpsc::UnboundedReceiver<PathEvent>,
    mut answers: mpsc::UnboundedReceiver<ProbeAnswered>,
) {
    let mut next = None;
    loop {
        let woken = wake.notified();
        tokio::select! {
            event = events.recv() => {
                let (Some(event), Some(driver)) = (event, driver.upgrade()) else { return };
                driver.event(event);
            }
            answer = answers.recv() => {
                let (Some(answer), Some(driver)) = (answer, driver.upgrade()) else { return };
                driver.answered(answer);
            }
            () = sleep_until(next) => {}
            () = woken => {}
        }
        let Some(driver) = driver.upgrade() else {
            return;
        };
        next = driver.tick();
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::link::socket::{Datagrams, PacketKind, encode_delivered, parse};
    use crate::trust::{SharedTrustStore, TrustStore, TrustStorePairingUpdate};

    /// The real loopback network, with the path to one address that can
    /// be cut both ways: what is sent there and what comes from there is
    /// lost, as it is when a LAN goes away.
    struct Cut {
        inner: Arc<dyn Datagrams>,
        other: Mutex<Option<SocketAddr>>,
        cut: AtomicBool,
    }

    impl Cut {
        fn loopback() -> Arc<Self> {
            Arc::new(Self {
                inner: LinkSocket::bind_dual_stack("127.0.0.1:0".parse().unwrap(), None).unwrap(),
                other: Mutex::new(None),
                cut: AtomicBool::new(false),
            })
        }

        fn cuts(&self, addr: SocketAddr) -> bool {
            self.cut.load(Ordering::SeqCst) && *self.other.lock().unwrap() == Some(addr)
        }
    }

    impl Datagrams for Cut {
        fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()> {
            if self.cuts(to) {
                return Ok(());
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
            loop {
                match self.inner.poll_recv(cx, buf) {
                    Poll::Ready(Ok((_, from))) if self.cuts(from) => {}
                    other => return other,
                }
            }
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }
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

    /// A forwarder standing in for the relay: each host's packets for the
    /// other, behind the forwarding header, go to the other naming their
    /// sender.
    async fn forwarder(hosts: [(HostId, SocketAddr); 2]) -> SocketAddr {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = vec![0; 2048];
            loop {
                let Ok((len, from)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let Ok(PacketKind::Forwarded { host: dst, payload }) = parse(&buf[..len]) else {
                    continue;
                };
                let sender = hosts.iter().find(|(_, at)| *at == from);
                let to = hosts.iter().find(|(id, _)| *id == dst);
                if let (Some((sender, _)), Some((_, to))) = (sender, to) {
                    let _ = socket
                        .send_to(&encode_delivered(*sender, payload), to)
                        .await;
                }
            }
        });
        addr
    }

    /// Two hosts linked over loopback with a forwarder beside them, each
    /// with its path machines.
    struct Linked {
        laptop: HostId,
        desk: HostId,
        laptop_net: Arc<Cut>,
        desk_net: Arc<Cut>,
        laptop_paths: Arc<PathDriver>,
        desk_paths: Arc<PathDriver>,
        lan: SocketAddr,
        client: quinn::Connection,
        server: quinn::Connection,
        _endpoints: (quinn::Endpoint, quinn::Endpoint),
        _tasks: (JoinHandle<()>, JoinHandle<()>),
    }

    const RELAY: LinkId = LinkId(90);
    const LINK: LinkId = LinkId(1);

    async fn linked(relayed: bool) -> Linked {
        linked_dialling(relayed, false).await
    }

    /// As [`linked`], the laptop dialling the desk's address, as a pairing
    /// does, rather than the desk itself, when `by_address`.
    async fn linked_dialling(relayed: bool, by_address: bool) -> Linked {
        let desk_identity = DeviceIdentity::for_test(HostId::new_v4());
        let laptop_identity = DeviceIdentity::for_test(HostId::new_v4());
        let (laptop, desk) = (laptop_identity.host_id, desk_identity.host_id);
        let (laptop_net, desk_net) = (Cut::loopback(), Cut::loopback());
        let lan = desk_net.local_addr().unwrap();
        let laptop_addr = laptop_net.local_addr().unwrap();
        *laptop_net.other.lock().unwrap() = Some(lan);
        *desk_net.other.lock().unwrap() = Some(laptop_addr);
        let desk_socket = LinkSocket::bind(desk_net.clone(), desk).unwrap();
        let laptop_socket = LinkSocket::bind(laptop_net.clone(), laptop).unwrap();

        let mut transport = crate::identity::quic_transport_base();
        transport
            .keep_alive_interval(Some(Duration::from_millis(500)))
            .max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()));
        let transport = Arc::new(transport);
        let mut server_config = desk_identity
            .quic_server_config(
                trust_for(&laptop_identity),
                &crate::link::resumption::SessionTickets::in_memory(16),
            )
            .unwrap();
        server_config.transport_config(transport.clone());
        let desk_endpoint = endpoint(desk_socket.clone(), Some(server_config));
        let laptop_endpoint = endpoint(laptop_socket.clone(), None);
        let stand_in = if by_address {
            laptop_socket.stand_in_at(lan)
        } else {
            laptop_socket.set_carrying(desk, Path::Direct(lan));
            laptop_socket.stand_in(desk)
        };
        let mut config = laptop_identity
            .quic_client_config_for_peer(trust_for(&desk_identity), desk)
            .unwrap();
        config.transport_config(transport);
        let connect = async {
            laptop_endpoint
                .connect_with(config, stand_in, crate::link::quic::DEVICE_SERVER_NAME)
                .unwrap()
                .await
                .unwrap()
        };
        let accept = async { desk_endpoint.accept().await.unwrap().await.unwrap() };
        let (client, server) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(connect, accept)
        })
        .await
        .expect("handshake completes");
        // The handshake named the laptop, as a link's admission does.
        assert!(desk_socket.name(server.remote_address(), laptop));

        let relay = forwarder([(laptop, laptop_addr), (desk, lan)]).await;
        let (laptop_paths, laptop_task) = PathDriver::start(laptop_socket.clone(), None);
        let (desk_paths, desk_task) = PathDriver::start(desk_socket.clone(), None);
        for socket in [&laptop_socket, &desk_socket]
            .into_iter()
            .filter(|_| relayed)
        {
            socket.add_forwarding(RELAY, socket.stand_in_at(relay), PathKind::RelayUdp);
        }
        laptop_paths.link_up_at(LINK, desk, client.remote_address(), PathKind::Lan);
        desk_paths.link_up_at(LINK, laptop, server.remote_address(), PathKind::Lan);
        Linked {
            laptop,
            desk,
            laptop_net,
            desk_net,
            laptop_paths,
            desk_paths,
            lan,
            client,
            server,
            _endpoints: (laptop_endpoint, desk_endpoint),
            _tasks: (laptop_task, desk_task),
        }
    }

    impl Linked {
        fn cut(&self, cut: bool) {
            self.laptop_net.cut.store(cut, Ordering::SeqCst);
            self.desk_net.cut.store(cut, Ordering::SeqCst);
        }

        /// Waits until each end carries the link on `path` of the laptop's
        /// and its match at the desk.
        async fn carried(&self, laptop: Path, desk: impl Fn(Path) -> bool) {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let at_laptop = self.laptop_paths.carrying(self.desk);
                    let at_desk = self.desk_paths.carrying(self.laptop);
                    if at_laptop == Some(laptop) && at_desk.is_some_and(&desk) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "the laptop carries {:?} and the desk {:?}",
                    self.laptop_paths.carrying(self.desk),
                    self.desk_paths.carrying(self.laptop)
                )
            });
        }
    }

    /// One line said on the open stream comes back from the far end.
    async fn echoed(send: &mut quinn::SendStream, recv: &mut quinn::RecvStream, line: &[u8; 4]) {
        send.write_all(line).await.unwrap();
        let mut back = [0; 4];
        tokio::time::timeout(Duration::from_secs(10), recv.read_exact(&mut back))
            .await
            .expect("the echo comes back")
            .unwrap();
        assert_eq!(&back, line);
    }

    /// A link carried on the LAN moves to the relay when the LAN goes, and
    /// back when it returns, with one stream open throughout: quinn sends
    /// to the same stand-in all along, and the stream never notices.
    #[tokio::test(flavor = "multi_thread")]
    async fn path_moves_a_link_from_lan_to_relay_and_back_with_its_stream_open() {
        let linked = linked(true).await;
        let server = linked.server.clone();
        tokio::spawn(async move {
            let (mut send, mut recv) = server.accept_bi().await.unwrap();
            let mut line = [0; 4];
            while recv.read_exact(&mut line).await.is_ok() {
                send.write_all(&line).await.unwrap();
            }
        });
        let (mut send, mut recv) = linked.client.open_bi().await.unwrap();
        let stand_in = linked.client.remote_address();

        echoed(&mut send, &mut recv, b"lan1").await;
        linked
            .carried(Path::Direct(linked.lan), |path| {
                matches!(path, Path::Direct(_))
            })
            .await;

        linked.cut(true);
        linked
            .carried(Path::Forwarded(RELAY), |path| {
                path == Path::Forwarded(RELAY)
            })
            .await;
        echoed(&mut send, &mut recv, b"rly1").await;

        linked.cut(false);
        linked
            .carried(Path::Direct(linked.lan), |path| {
                matches!(path, Path::Direct(_))
            })
            .await;
        echoed(&mut send, &mut recv, b"lan2").await;

        assert_eq!(
            linked.client.remote_address(),
            stand_in,
            "quinn saw no move"
        );
        assert!(linked.client.close_reason().is_none(), "the link stayed up");
    }

    /// A link the laptop dialled at the desk's address, as a pairing does,
    /// still gives the desk its machine, and a network change tells it.
    #[tokio::test(flavor = "multi_thread")]
    async fn path_gives_the_peer_of_a_link_dialled_by_address_its_machine() {
        let linked = linked_dialling(false, true).await;
        assert_eq!(linked.laptop_paths.machines(), 1);
        assert_eq!(linked.laptop_paths.network_changed(Vec::new()), 1);
        assert!(linked.client.close_reason().is_none(), "the link stayed up");
    }

    /// Only the answer to the probe outstanding on a path carries it: an
    /// answer to a probe sent before a network change, a repeated answer
    /// and one past the probe's timeout all count for nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn path_counts_only_the_answer_to_the_probe_outstanding() {
        let linked = linked(false).await;
        let paths = &linked.laptop_paths;
        let desk = linked.desk;
        // Wait out the first carry, then make every path suspect with the
        // network gone quiet, so nothing answers on its own: with no relay,
        // nothing carries the link until the LAN answers again.
        linked
            .carried(Path::Direct(linked.lan), |path| {
                matches!(path, Path::Direct(_))
            })
            .await;
        linked.cut(true);
        let lan = Path::Direct(linked.lan);
        let nonce_on = |paths: &PathDriver| {
            paths.lock().machines[&desk]
                .probes
                .get(&lan)
                .map(|probe| probe.nonce)
        };
        // A network change: every path is probed afresh.
        paths.network_changed(Vec::new());
        let before = nonce_on(paths).expect("the LAN is probed again");
        paths.network_changed(Vec::new());
        let outstanding = nonce_on(paths).expect("and again");
        assert_ne!(before, outstanding);

        paths.answered(ProbeAnswered {
            from: desk,
            nonce: before,
        });
        assert!(
            nonce_on(paths) == Some(outstanding),
            "an answer to an earlier probe matches nothing"
        );
        paths.answered(ProbeAnswered {
            from: linked.laptop,
            nonce: outstanding,
        });
        assert_eq!(
            nonce_on(paths),
            Some(outstanding),
            "nor one from another host"
        );

        // Past its timeout, the right answer is too late.
        if let Some(probe) = paths
            .lock()
            .machines
            .get_mut(&desk)
            .unwrap()
            .probes
            .get_mut(&lan)
        {
            probe.sent = Instant::now() - Duration::from_millis(PROBE_TIMEOUT_MS);
        }
        paths.answered(ProbeAnswered {
            from: desk,
            nonce: outstanding,
        });
        assert_eq!(nonce_on(paths), None, "the late answer is used up");
        assert_eq!(paths.carrying(desk), None, "and carries nothing");

        // The answer in time is the one that counts, once.
        paths.network_changed(Vec::new());
        let fresh = nonce_on(paths).expect("probed again");
        paths.answered(ProbeAnswered {
            from: desk,
            nonce: fresh,
        });
        assert_eq!(
            paths.carrying(desk),
            Some(lan),
            "the answered path carries the link"
        );
        paths.answered(ProbeAnswered {
            from: desk,
            nonce: fresh,
        });
        assert_eq!(nonce_on(paths), None, "a repeated answer matches nothing");
    }
}
