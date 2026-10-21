//! Forwarding a link's stream to another host: the driver of the
//! forwarding core.
//!
//! [`relay_core::Relay`] decides which streams are refused and which are
//! spliced, and which spliced streams a lapsed plan stops. This driver mints
//! each stream's name, resets what the core refuses before a byte of it is
//! copied, opens the far stream on the link the core names and copies the
//! bytes both ways on tokio's workers. A relay shares one core across every
//! account; a daemon has one of its own.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use core_ring::{DumpKind, DumpLimit};
use daemon_core::DumpReason;
use daemon_core::payload::ForwardRefusal;
use relay_core::{Input, Output, Relay, StreamId};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;
use wire::pb;

use super::forwarder::{ForwardLinks, Forwarder, HoldLimits, LinkLimits};
use super::socket::{ForwardSink, LinkSocket};
use super::{ByteStream, OpenError};
use crate::HostId;
use crate::routing::{LinkId, LinkOutputTx, LinkRegistry};
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
    next_stream: AtomicU64,
    /// Where a dump the core asks for is written; unset only in tests that
    /// never read one.
    reports: OnceLock<PathBuf>,
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
    fn reason(&mut self, reason: &DumpReason, now_ms: u64) -> Option<String> {
        let kind = match reason {
            DumpReason::Violation(violation) => DumpKind::Violation(violation.invariant.into()),
            DumpReason::OverBudget { .. } => DumpKind::OverBudget,
        };
        let suppressed = self.0.admit(kind, now_ms)?;
        Some(core_ring::with_suppressed(
            format!("{RELAY_CORE_DUMP}{reason}"),
            suppressed,
        ))
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
            next_stream: AtomicU64::new(1),
            reports: OnceLock::new(),
        })
    }

    /// Writes the dumps the core asks for into `dir`: a relay's directory
    /// from its configuration, a daemon's its installation's reports.
    pub(crate) fn report_to(&self, dir: PathBuf) {
        let _ = self.reports.set(dir);
    }

    /// Forwards the packets hosts send `socket` to forward, between the
    /// pairs the core installs. From then on the core is stepped through
    /// the forwarder, and no stream is spliced.
    pub(crate) fn forward_on(self: &Arc<Self>, socket: Arc<LinkSocket>) {
        let links = Arc::new(RelayLinks {
            socket: socket.clone(),
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
            {
                let core = lock(&self.core);
                lock(&self.history).window.record(&core, &input);
            }
            let gone = match input {
                Input::LinkDown { link } => Some(link),
                _ => None,
            };
            match self.forwarding.get() {
                // The forwarder has reported the dumps among these.
                Some(forwarding) => out.extend(
                    forwarding
                        .forwarder
                        .step(input)
                        .into_iter()
                        .filter(|output| !matches!(output, Output::Dump { .. })),
                ),
                None => {
                    let mut stepped = Vec::new();
                    lock(&self.core).step(input, &mut stepped);
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

    /// A link the core holds as up came from `stand_in` on the relay's
    /// socket: its packets are forwarded from now on, those held for it
    /// first, and refusals are written to `tx`.
    pub(crate) fn admitted(
        &self,
        link: daemon_core::LinkId,
        tx: LinkOutputTx,
        stand_in: Option<SocketAddr>,
    ) {
        let (Some(forwarding), Some(stand_in)) = (self.forwarding.get(), stand_in) else {
            return;
        };
        forwarding.links.admit(link, tx, stand_in);
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

    fn forwards_packets(&self) -> bool {
        self.forwarding.get().is_some()
    }

    /// Writes one dump the core asked for, unless one like it was written
    /// this interval: the reason, the core's state when the window began,
    /// every input stepped since and the state now. The relay core sees
    /// ids, prefaces and tiers, never a token (tokens are checked before a
    /// link is admitted), so the file holds no secret.
    fn report(&self, reason: &DumpReason) {
        let now_ms = self.now_ms();
        let (reason, window) = {
            let mut history = lock(&self.history);
            let Some(reason) = history.dumps.reason(reason, now_ms) else {
                return;
            };
            (reason, history.window.clone())
        };
        let Some(dir) = self.reports.get() else {
            tracing::error!(%reason, "the relay core asked for a dump, with nowhere to write it");
            return;
        };
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

    /// The driver is done with `stream`.
    fn ended(&self, stream: StreamId) {
        self.step([Input::StreamEnded { stream }]);
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

/// The relay's links as the forwarder sends on them: each admitted link's
/// stand-in on the socket and its control writer.
struct RelayLinks {
    socket: Arc<LinkSocket>,
    relaying: Weak<Relaying>,
    admitted: Mutex<Admitted>,
}

#[derive(Default)]
struct Admitted {
    links: HashMap<daemon_core::LinkId, (LinkOutputTx, SocketAddr)>,
    senders: HashMap<SocketAddr, daemon_core::LinkId>,
}

impl RelayLinks {
    fn admit(&self, link: daemon_core::LinkId, tx: LinkOutputTx, stand_in: SocketAddr) {
        let mut admitted = lock(&self.admitted);
        admitted.links.insert(link, (tx, stand_in));
        admitted.senders.insert(stand_in, link);
    }

    fn forget(&self, link: daemon_core::LinkId) {
        let mut admitted = lock(&self.admitted);
        if let Some((_, stand_in)) = admitted.links.remove(&link)
            && admitted.senders.get(&stand_in) == Some(&link)
        {
            admitted.senders.remove(&stand_in);
        }
    }

    fn is_sender(&self, stand_in: SocketAddr) -> bool {
        lock(&self.admitted).senders.contains_key(&stand_in)
    }

    fn sender(&self, stand_in: SocketAddr) -> Option<daemon_core::LinkId> {
        lock(&self.admitted).senders.get(&stand_in).copied()
    }
}

impl ForwardLinks for RelayLinks {
    fn send(&self, link: daemon_core::LinkId, datagram: &[u8]) -> std::io::Result<()> {
        let stand_in = lock(&self.admitted)
            .links
            .get(&link)
            .map(|(_, stand_in)| *stand_in)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotConnected, "the link is gone")
            })?;
        self.socket.send_raw(stand_in, datagram)
    }

    fn refuse(&self, link: daemon_core::LinkId, dst: HostId, reason: ForwardRefusal) {
        let Some(tx) = lock(&self.admitted)
            .links
            .get(&link)
            .map(|(tx, _)| tx.clone())
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
            Some(link) => self.forwarder.on_packet(link, dst, payload),
            None => self.forwarder.hold(sender, dst, payload),
        };
    }
}

#[derive(Clone)]
pub(crate) struct Piper {
    links: Arc<LinkRegistry>,
}

impl Piper {
    pub(crate) fn new(links: Arc<LinkRegistry>) -> Self {
        Self { links }
    }

    pub(crate) async fn pipe(
        &self,
        origin: LinkId,
        preface: pb::StreamPreface,
        mut incoming: ByteStream,
    ) -> Result<JoinHandle<()>, pb::StreamRefusal> {
        let relaying = self.links.relaying().clone();
        // A relay forwards its hosts' packets end to end and carries no
        // stream between them.
        if relaying.forwards_packets() {
            let _ = incoming.reset(pb::StreamRefusal::NoRoute).await;
            return Err(pb::StreamRefusal::NoRoute);
        }
        let stream = StreamId(relaying.next_stream.fetch_add(1, Ordering::Relaxed));
        let decided = relaying.step([
            Input::Tick {
                now_ms: relaying.now_ms(),
            },
            Input::StreamOpened {
                stream,
                link: origin.core(),
                preface: preface.clone(),
            },
        ]);
        // The core answers every stream opened with a refusal or a splice.
        let spliced = decided.iter().find_map(|output| match *output {
            Output::Splice { to_link, .. } => Some(Ok(to_link)),
            Output::RefuseStream { reason, .. } => Some(Err(reason)),
            _ => None,
        });
        let to_link = match spliced.unwrap_or(Err(pb::StreamRefusal::NoRoute)) {
            Ok(to_link) => to_link,
            Err(reason) => {
                let _ = incoming.reset(reason).await;
                relaying.ended(stream);
                return Err(reason);
            }
        };
        let refuse = |reason| {
            relaying.ended(stream);
            reason
        };
        let Some(outgoing) = self.links.native_carrier_of(to_link).await else {
            let _ = incoming.reset(pb::StreamRefusal::NoRoute).await;
            return Err(refuse(pb::StreamRefusal::NoRoute));
        };

        // The opener's handshake rides inside the stream, whatever its
        // preface claims: a relay vouches for nobody.
        let preface = pb::StreamPreface {
            dst: preface.dst,
            plain: false,
        };
        let opened = match outgoing.open_stream(preface).await {
            Ok(mut stream) => stream.accepted().await.map(|()| stream),
            Err(error) => Err(error),
        };
        let outgoing = match opened {
            Ok(stream) => stream,
            Err(error) => {
                let reason = match error {
                    OpenError::Refused(reason) => reason,
                    OpenError::LinkClosed | OpenError::Io(_) => pb::StreamRefusal::NoRoute,
                };
                let _ = incoming.reset(reason).await;
                return Err(refuse(reason));
            }
        };
        if incoming.flush().await.is_err() {
            return Err(refuse(pb::StreamRefusal::NoRoute));
        }
        let copying = relaying.clone();
        Ok(tokio::spawn(async move {
            let (mut incoming_read, mut incoming_write) = tokio::io::split(incoming);
            let (mut outgoing_read, mut outgoing_write) = tokio::io::split(outgoing);
            // Each direction runs to its own end, whatever becomes of the
            // other. A host refusing a stream writes its last words, a TLS
            // alert saying why, and stops reading at once: forwarding
            // toward it fails then, and its words must still reach the far
            // end.
            tokio::join!(
                forward(&mut incoming_read, &mut outgoing_write),
                forward(&mut outgoing_read, &mut incoming_write),
            );
            copying.ended(stream);
        }))
    }
}

/// Copies `from` into `to` until `from` ends or either fails, then closes
/// the writing side of `to`.
async fn forward(from: &mut (impl AsyncRead + Unpin), to: &mut (impl AsyncWrite + Unpin)) {
    let _ = tokio::io::copy(from, to).await;
    let _ = to.shutdown().await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    use super::*;
    use crate::link::{ControlSink, ControlSource, LinkCarrier as NativeLinkCarrier, QuicCarrier};
    use crate::routing::{
        Capabilities, Host, LinkAdmission, LinkCarrier, LinkProperties, LinkRole,
    };

    fn host(id: u128) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                cloud_relay: false,
                kinds: vec![wire::Kind::ClaudeSdk as i32],
            },
            signed_in: Some(false),
            platform: None,
        }
    }

    struct CarrierPair {
        connector: Arc<QuicCarrier>,
        acceptor: Arc<QuicCarrier>,
        _connector_control: (ControlSink, ControlSource),
        _acceptor_control: (ControlSink, ControlSource),
    }

    async fn carriers() -> CarrierPair {
        let (connector, acceptor) =
            crate::link::quic::stream_pair(crate::link::framed::FramedKind::RelayTcp).await;
        let (mut connector_sink, connector_source) = connector.control();
        let (acceptor_sink, mut acceptor_source) = acceptor.control();
        crate::link::write_message(&mut connector_sink, &pb::Message { body: None })
            .await
            .unwrap();
        crate::link::read_message(&mut acceptor_source)
            .await
            .unwrap();
        CarrierPair {
            connector,
            acceptor,
            _connector_control: (connector_sink, connector_source),
            _acceptor_control: (acceptor_sink, acceptor_source),
        }
    }

    async fn register_link(
        links: &LinkRegistry,
        peer: u128,
        admission: LinkAdmission,
        carrier: Option<Arc<dyn NativeLinkCarrier>>,
    ) -> LinkId {
        let link = LinkId::new(HostId::from_u128(peer));
        let (outgoing, _) = mpsc::channel(8);
        links
            .register_with_details(
                link,
                host(peer),
                outgoing,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission,
                    carrier: LinkCarrier::Direct,
                    runtime_instance: crate::routing::RuntimeInstance::random(),
                    direct_order: None,
                },
                &[],
                carrier,
            )
            .await
            .expect("piper test links are unique");
        link
    }

    async fn incoming(
        opener: Arc<QuicCarrier>,
        receiver: Arc<QuicCarrier>,
        destination: HostId,
    ) -> (
        tokio::task::JoinHandle<Result<ByteStream, OpenError>>,
        pb::StreamPreface,
        ByteStream,
    ) {
        let preface = pb::StreamPreface {
            dst: destination.as_bytes().to_vec(),
            plain: false,
        };
        // The open is done once the relay has answered it: a QUIC stream
        // is handed back as soon as its preface is written.
        let opening = tokio::spawn({
            let preface = preface.clone();
            async move {
                let mut stream = opener.open_stream(preface).await?;
                stream.accepted().await?;
                Ok(stream)
            }
        });
        let (received, stream) =
            tokio::time::timeout(Duration::from_secs(1), receiver.accept_stream())
                .await
                .expect("timed out waiting for the relay-side stream")
                .unwrap();
        assert_eq!(received, preface);
        (opening, preface, stream)
    }

    async fn assert_refusal(
        piper: &Piper,
        origin_link: LinkId,
        opener: Arc<QuicCarrier>,
        receiver: Arc<QuicCarrier>,
        destination: HostId,
        expected: pb::StreamRefusal,
    ) {
        let (opening, preface, stream) = incoming(opener, receiver, destination).await;
        assert!(matches!(
            piper.pipe(origin_link, preface, stream).await,
            Err(actual) if actual == expected
        ));
        assert!(matches!(
            opening.await.unwrap(),
            Err(OpenError::Refused(actual)) if actual == expected
        ));
    }

    #[tokio::test]
    async fn every_policy_refusal_is_reset_before_bytes_are_copied() {
        let local = HostId::from_u128(1);

        let links = Arc::new(LinkRegistry::new(local));
        let origin = register_link(&links, 2, LinkAdmission::PinnedKey, None).await;
        let piper = Piper::new(links.clone());
        let pair = carriers().await;
        assert_refusal(
            &piper,
            origin,
            pair.connector.clone(),
            pair.acceptor.clone(),
            local,
            pb::StreamRefusal::NotAdjacent,
        )
        .await;
        assert_refusal(
            &piper,
            origin,
            pair.connector.clone(),
            pair.acceptor.clone(),
            HostId::from_u128(99),
            pb::StreamRefusal::NoRoute,
        )
        .await;
    }

    /// A stream piped between two pinned-key links: the task copying it,
    /// the origin's end, the destination's end, and the carriers under them.
    async fn piped() -> (
        JoinHandle<Result<(), tokio::task::JoinError>>,
        ByteStream,
        ByteStream,
        (CarrierPair, CarrierPair),
    ) {
        let (copy_task, origin, destination, carriers, _) =
            piped_between(LinkAdmission::PinnedKey).await;
        (copy_task, origin, destination, carriers)
    }

    /// A stream piped between two links admitted alike, and the registry
    /// and origin link it was opened through.
    async fn piped_between(
        admission: LinkAdmission,
    ) -> (
        JoinHandle<Result<(), tokio::task::JoinError>>,
        ByteStream,
        ByteStream,
        (CarrierPair, CarrierPair),
        (Arc<LinkRegistry>, LinkId),
    ) {
        let local = HostId::from_u128(1);
        let links = Arc::new(LinkRegistry::new(local));
        let origin_link = register_link(&links, 2, admission, None).await;
        let destination_pair = carriers().await;
        register_link(
            &links,
            3,
            admission,
            Some(destination_pair.connector.clone()),
        )
        .await;
        let piper = Piper::new(links.clone());
        let origin_pair = carriers().await;
        let (opening, preface, incoming) = incoming(
            origin_pair.connector.clone(),
            origin_pair.acceptor.clone(),
            HostId::from_u128(3),
        )
        .await;

        let copy_task = tokio::spawn(async move {
            piper
                .pipe(origin_link, preface, incoming)
                .await
                .unwrap()
                .await
        });
        let (received, mut destination_stream) = tokio::time::timeout(
            Duration::from_secs(1),
            destination_pair.acceptor.accept_stream(),
        )
        .await
        .expect("timed out waiting for the destination stream")
        .unwrap();
        assert_eq!(received.dst, HostId::from_u128(3).as_bytes());
        tokio::time::timeout(Duration::from_secs(1), destination_stream.flush())
            .await
            .expect("timed out accepting the destination stream")
            .unwrap();
        let origin_stream = tokio::time::timeout(Duration::from_secs(1), opening)
            .await
            .expect("timed out accepting the origin stream")
            .unwrap()
            .unwrap();
        (
            copy_task,
            origin_stream,
            destination_stream,
            (origin_pair, destination_pair),
            (links, origin_link),
        )
    }

    #[tokio::test]
    async fn two_pinned_key_links_pipe_with_no_tier_anywhere() {
        let (copy_task, mut origin_stream, mut destination_stream, _carriers) = piped().await;

        origin_stream.write_all(b"outbound").await.unwrap();
        let mut outbound = [0; 8];
        tokio::time::timeout(
            Duration::from_secs(1),
            destination_stream.read_exact(&mut outbound),
        )
        .await
        .expect("timed out copying toward the destination")
        .unwrap();
        assert_eq!(&outbound, b"outbound");

        destination_stream.write_all(b"inbound").await.unwrap();
        let mut inbound = [0; 7];
        tokio::time::timeout(
            Duration::from_secs(1),
            origin_stream.read_exact(&mut inbound),
        )
        .await
        .expect("timed out copying toward the origin")
        .unwrap();
        assert_eq!(&inbound, b"inbound");

        destination_stream.shutdown().await.unwrap();
        origin_stream.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), copy_task)
            .await
            .expect("the piper copy task must end when both streams end")
            .unwrap()
            .unwrap();
    }

    /// One end that stops sending does not cut off what the other end is
    /// still sending it: a host refusing a stream says why and stops, and
    /// the far end must still read why.
    #[tokio::test]
    async fn an_end_that_stops_sending_still_reads_all_the_other_end_sent() {
        let (copy_task, mut origin_stream, mut destination_stream, _carriers) = piped().await;
        // More than the carriers buffer, so most of it is still on its way
        // when the origin stops sending.
        let sent = (0..4 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let sending = tokio::spawn({
            let sent = sent.clone();
            async move {
                destination_stream.write_all(&sent).await.unwrap();
                destination_stream.shutdown().await.unwrap();
                let mut rest = Vec::new();
                destination_stream.read_to_end(&mut rest).await.unwrap();
                rest
            }
        });

        origin_stream.shutdown().await.unwrap();
        let mut received = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            origin_stream.read_to_end(&mut received),
        )
        .await
        .expect("timed out reading what the destination sent")
        .unwrap();
        assert_eq!(received.len(), sent.len());
        assert!(
            received == sent,
            "the origin read what the destination sent"
        );
        assert!(sending.await.unwrap().is_empty());
        tokio::time::timeout(Duration::from_secs(1), copy_task)
            .await
            .expect("the piper copy task must end when both streams end")
            .unwrap()
            .unwrap();
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
        relaying.report_to(reports.path().to_owned());
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
            invariant: "a splice only joins two live links",
            detail: "link 9 is down".into(),
        });
        let mut dumps = Dumps::default();
        let first = dumps.reason(&broke, 1_000).unwrap();
        assert!(first.starts_with(RELAY_CORE_DUMP), "{first}");
        for at in [1_001, 2_000, 1_000 + DumpLimit::INTERVAL_MS - 1] {
            assert_eq!(dumps.reason(&broke, at), None);
        }
        let second = dumps
            .reason(&broke, 1_000 + DumpLimit::INTERVAL_MS)
            .unwrap();
        assert!(
            second.ends_with("(3 more like it were not dumped since the last)"),
            "{second}"
        );
    }
}
