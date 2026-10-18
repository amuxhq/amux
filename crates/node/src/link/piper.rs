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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use relay_core::{Input, Output, Relay, StreamId};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::task::{AbortHandle, JoinHandle};
use wire::pb;

use super::{ByteStream, OpenError};
use crate::HostId;
use crate::routing::{LinkId, LinkRegistry};

/// Prefixes the reason the forwarding core asked for a dump.
pub const RELAY_CORE_DUMP: &str = "the relay core: ";

/// The forwarding core and the copies it started, under one lock: a cut
/// reaches a copy atomically with the step that decided it.
pub(crate) struct Relaying {
    inner: std::sync::Mutex<Inner>,
    /// The clock the core's rate window is measured by.
    epoch: Instant,
    next_stream: AtomicU64,
}

struct Inner {
    relay: Relay,
    pipes: HashMap<StreamId, Pipe>,
}

/// A spliced stream as the driver holds it.
enum Pipe {
    /// Its far end is being opened.
    Opening,
    /// The core stopped it while its far end was opening.
    Cut(pb::StreamRefusal),
    /// Its bytes are being copied.
    Running(AbortHandle),
}

impl Relaying {
    pub(crate) fn new(own: HostId) -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(Inner {
                relay: Relay::new(daemon_core::HostId(own.into_bytes())),
                pipes: HashMap::new(),
            }),
            epoch: Instant::now(),
            next_stream: AtomicU64::new(1),
        })
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Steps each input in turn and does what the core asked that this
    /// driver does under the lock: a cut stops its copy, a splice is held
    /// as opening, a dump is reported. Refusals and splices are returned
    /// for the caller to act on.
    pub(crate) fn step(&self, inputs: impl IntoIterator<Item = Input>) -> Vec<Output> {
        let mut inner = self.inner();
        let mut out = Vec::new();
        for input in inputs {
            inner.relay.step(input, &mut out);
        }
        out.retain(|output| match output {
            Output::Cut { stream, reason } => {
                match inner.pipes.get_mut(stream) {
                    Some(Pipe::Running(pipe)) => {
                        pipe.abort();
                        inner.pipes.remove(stream);
                    }
                    Some(pipe @ Pipe::Opening) => *pipe = Pipe::Cut(*reason),
                    Some(Pipe::Cut(_)) | None => {}
                }
                false
            }
            Output::Splice { stream, .. } => {
                inner.pipes.insert(*stream, Pipe::Opening);
                true
            }
            Output::Dump { reason } => {
                tracing::error!(reason = %format_args!("{RELAY_CORE_DUMP}{reason}"), "the relay core asked for a dump");
                false
            }
            Output::Refuse { .. }
            | Output::RefuseLink { .. }
            | Output::Displace { .. }
            | Output::Neighbor { .. } => true,
        });
        out
    }

    /// The hosts on `user`'s account holding a link here.
    pub(crate) fn neighbors(&self, user: relay_core::UserId) -> Vec<daemon_core::HostId> {
        self.inner().relay.neighbors(user)
    }

    /// The driver is done with `stream`.
    fn ended(&self, stream: StreamId) {
        self.inner().pipes.remove(&stream);
        self.step([Input::StreamEnded { stream }]);
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
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
            Output::Refuse { reason, .. } => Some(Err(reason)),
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
        let (start, started) = tokio::sync::oneshot::channel::<()>();
        let copying = relaying.clone();
        let pipe = tokio::spawn(async move {
            if started.await.is_err() {
                return;
            }
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
        });
        {
            // A link that lapsed while the far end was opening had this
            // stream cut then: it does not start, or it carries on unpaid.
            let mut inner = relaying.inner();
            match inner.pipes.remove(&stream) {
                Some(Pipe::Opening) => {
                    inner
                        .pipes
                        .insert(stream, Pipe::Running(pipe.abort_handle()));
                }
                Some(Pipe::Cut(reason)) => {
                    pipe.abort();
                    return Err(reason);
                }
                // Only this call starts the stream's copy.
                Some(Pipe::Running(_)) | None => {
                    pipe.abort();
                    return Err(pb::StreamRefusal::NoRoute);
                }
            }
        }
        let _ = start.send(());
        Ok(pipe)
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
    use crate::link::{
        CarrierKind, ControlSink, ControlSource, LinkCarrier as NativeLinkCarrier, MuxCarrier,
        MuxRole,
    };
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
        connector: Arc<MuxCarrier>,
        acceptor: Arc<MuxCarrier>,
        _connector_control: (ControlSink, ControlSource),
        _acceptor_control: (ControlSink, ControlSource),
    }

    async fn carriers() -> CarrierPair {
        let (connector_io, acceptor_io) = tokio::io::duplex(2 * 1024 * 1024);
        let connector = Arc::new(MuxCarrier::new(
            connector_io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));
        let acceptor = Arc::new(MuxCarrier::new(
            acceptor_io,
            MuxRole::Acceptor,
            CarrierKind::RelayTcp,
        ));
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
        opener: Arc<MuxCarrier>,
        receiver: Arc<MuxCarrier>,
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
        let opening = tokio::spawn({
            let preface = preface.clone();
            async move { opener.open_stream(preface).await }
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
        opener: Arc<MuxCarrier>,
        receiver: Arc<MuxCarrier>,
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

        let free_links = Arc::new(LinkRegistry::new(local));
        let free_origin = register_link(
            &free_links,
            2,
            LinkAdmission::CloudToken {
                tier: crate::Tier::Free,
            },
            None,
        )
        .await;
        let free_piper = Piper::new(free_links);
        let free_pair = carriers().await;
        assert_refusal(
            &free_piper,
            free_origin,
            free_pair.connector.clone(),
            free_pair.acceptor.clone(),
            HostId::from_u128(99),
            pb::StreamRefusal::PaymentRequired,
        )
        .await;

        let rate_links = Arc::new(LinkRegistry::new(local));
        let rate_origin = register_link(&rate_links, 2, LinkAdmission::PinnedKey, None).await;
        let rate_piper = Piper::new(rate_links);
        let rate_pair = carriers().await;
        for _ in 0..relay_core::RATE_LIMIT {
            assert_refusal(
                &rate_piper,
                rate_origin,
                rate_pair.connector.clone(),
                rate_pair.acceptor.clone(),
                HostId::from_u128(99),
                pb::StreamRefusal::NoRoute,
            )
            .await;
        }
        assert_refusal(
            &rate_piper,
            rate_origin,
            rate_pair.connector.clone(),
            rate_pair.acceptor.clone(),
            HostId::from_u128(99),
            pb::StreamRefusal::RateLimited,
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

    /// A link that lapses to free while a stream through it waits on the
    /// far end's acceptance closes its pipes before that pipe registers;
    /// the pipe must not start then, or it carries on unpaid.
    #[tokio::test]
    async fn a_link_lapsing_while_its_stream_opens_ends_that_stream_too() {
        let local = HostId::from_u128(1);
        let links = Arc::new(LinkRegistry::new(local));
        let paid = LinkAdmission::CloudToken {
            tier: crate::Tier::Pro,
        };
        let origin_link = register_link(&links, 2, paid, None).await;
        let destination_pair = carriers().await;
        register_link(&links, 3, paid, Some(destination_pair.connector.clone())).await;
        let piper = Piper::new(links.clone());
        let origin_pair = carriers().await;
        let (_opening, preface, incoming) = incoming(
            origin_pair.connector.clone(),
            origin_pair.acceptor.clone(),
            HostId::from_u128(3),
        )
        .await;
        let piping = tokio::spawn({
            let piper = piper.clone();
            async move { piper.pipe(origin_link, preface, incoming).await }
        });
        let (_, mut destination_stream) = tokio::time::timeout(
            Duration::from_secs(1),
            destination_pair.acceptor.accept_stream(),
        )
        .await
        .expect("timed out waiting for the destination stream")
        .unwrap();
        // The pipe is waiting for this acceptance when the link lapses.
        links
            .update_cloud_tier(&origin_link, crate::Tier::Free)
            .await;
        destination_stream.flush().await.unwrap();
        let piped = tokio::time::timeout(Duration::from_secs(1), piping)
            .await
            .expect("timed out piping")
            .unwrap();
        assert!(
            matches!(piped, Err(pb::StreamRefusal::PaymentRequired)),
            "a pipe admitted before its link lapsed does not start"
        );
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

    /// A link that lapses to free stops carrying the streams it opened
    /// while it was paid for.
    #[tokio::test]
    async fn a_link_lapsing_stops_the_streams_already_running_through_it() {
        let (copy_task, _origin, mut destination_stream, _carriers, (links, origin_link)) =
            piped_between(LinkAdmission::CloudToken {
                tier: crate::Tier::Pro,
            })
            .await;
        links
            .update_cloud_tier(&origin_link, crate::Tier::Free)
            .await;
        let stopped = tokio::time::timeout(Duration::from_secs(1), copy_task)
            .await
            .expect("the copy stops when its link lapses")
            .unwrap();
        assert!(stopped.is_err_and(|error| error.is_cancelled()));
        let mut rest = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            destination_stream.read_to_end(&mut rest),
        )
        .await
        .expect("the far end sees the stream end")
        .ok();
    }
}
