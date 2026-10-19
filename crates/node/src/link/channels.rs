//! Tonic channels carried by independent native link streams.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock, Weak};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::future::BoxFuture;
use rustls::pki_types::ServerName;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use tonic::codegen::http;
use tonic::transport::{Channel, Endpoint};
use wire::pb;

use super::{ByteStream, CarrierKind, LinkCarrier, OpenError};
use crate::HostId;
use crate::dispatcher::TunnelDispatcher;
use crate::driver::{CoreLinks, core_error};
use crate::identity::{DeviceIdentity, IdentityError};
use crate::routing::{LinkId, LinkRegistry, Route, core_host};
use crate::transport::{Compressed, channel_from_single_io, configure_tonic_endpoint_keepalive};
use crate::trust::SharedTrustStore;

const CHANNEL_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP2_FRAME_HEADER_LEN: usize = 9;
const HTTP2_DATA_FRAME: u8 = 0;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelClass {
    Calls,
    /// Session subscriptions, on a channel of their own so that they
    /// neither wait behind nor hold up the host's calls.
    Session,
    Bulk,
}

impl ChannelClass {
    /// Whether one channel of this class serves every caller to a host
    /// while its connection lasts.
    fn shared(self) -> bool {
        matches!(self, Self::Calls | Self::Session)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(crate) struct ChannelKey {
    pub(crate) peer: HostId,
    pub(crate) route: Route,
    pub(crate) class: ChannelClass,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("host {host_id} is not reachable")]
    NoRoute { host_id: HostId },
    #[error(
        "Pairing could not reach this host. Check that both devices are online and signed in to the same cloud account."
    )]
    CloudPairingUnavailable,
    #[error("stream refused: {0:?}")]
    Refused(pb::StreamRefusal),
    #[error("no live link to host {host_id}")]
    LinkUnavailable { host_id: HostId },
    #[error("channel handshake failed: {0}")]
    Handshake(String),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("channel TLS failed: {0}")]
    Tls(String),
}

/// How a stream to a host is secured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Handshake {
    /// Whatever the route allows: none on a direct link of our own.
    Any,
    /// The pinned TLS handshake inside the stream, whatever the route.
    Pinned,
    /// None: the link the stream rides authenticated both ends.
    None,
}

#[derive(Clone)]
struct ChannelSecurity {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
}

/// A calls channel kept for reuse, and whether the one connection under it
/// has ended. A channel over a link stream cannot reconnect, so once its
/// connection is gone it is dropped and the next call opens a new stream,
/// even where the route it rode is still up: the far host may have closed
/// the stream on its own, as it does to a host it stops trusting.
#[derive(Clone)]
struct Cached {
    channel: Channel,
    ended: Arc<Ending>,
}

/// Whether the connection under a channel has ended, and the core to tell
/// when it does, once the channel has been reported open.
#[derive(Default)]
struct Ending {
    ended: AtomicBool,
    tell: OnceLock<(CoreLinks, daemon_core::ChannelId)>,
}

impl Ending {
    fn has_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    fn end(&self) {
        if !self.ended.swap(true, Ordering::SeqCst) {
            self.tell_ended();
        }
    }

    /// Tells `core` when the channel ends, or now if it already has. A
    /// channel ending as it is armed may be told twice; the core forgets a
    /// channel once.
    fn arm(&self, core: CoreLinks, channel: daemon_core::ChannelId) {
        let _ = self.tell.set((core, channel));
        if self.has_ended() {
            self.tell_ended();
        }
    }

    fn tell_ended(&self) {
        if let Some((core, channel)) = self.tell.get() {
            core.closed(*channel, daemon_core::payload::ChannelError::Ended);
        }
    }

    /// Tells the core the far host took this host's certificate on the
    /// channel.
    fn tell_answered(&self) {
        if let Some((core, channel)) = self.tell.get() {
            core.report(daemon_core::Input::ChannelAnswered { channel: *channel });
        }
    }

    /// Tells the core the far host refused this host's certificate as no
    /// longer trusted.
    fn tell_revoked(&self, peer: HostId) {
        if let Some((core, _)) = self.tell.get() {
            core.report(daemon_core::Input::PeerRevoked {
                host: core_host(peer),
            });
        }
    }
}

pub struct ChannelPool {
    by_key: RwLock<HashMap<ChannelKey, Cached>>,
    /// One opener per shared channel: callers who ask while it is being
    /// opened wait for it rather than opening their own, which at a launch
    /// every agent's source would otherwise do at once.
    opening: std::sync::Mutex<HashMap<ChannelKey, Arc<tokio::sync::Mutex<()>>>>,
    lifetimes: RwLock<HashMap<ChannelKey, Vec<Weak<CancellationToken>>>>,
    links: Arc<LinkRegistry>,
    security: Option<ChannelSecurity>,
    handshake_timeout: Duration,
    bulk_response_holds: std::sync::Mutex<HashMap<HostId, PendingBulkResponseHold>>,
    /// Hosts no channel opens to while the link stays up: a fault a test
    /// network injects.
    refused: std::sync::Mutex<HashSet<HostId>>,
}

/// A channel the core said to open, whose outcome the core is told exactly
/// once. One dropped before it was told, as a caller that gives up waiting
/// drops its open, is told it was abandoned.
struct Telling {
    core: CoreLinks,
    channel: daemon_core::ChannelId,
    told: bool,
}

impl Telling {
    fn opened(mut self, link: LinkId, key: ChannelKey) -> (CoreLinks, daemon_core::ChannelId) {
        self.told = true;
        self.core.opened(self.channel, link, key.peer, key.class);
        (self.core.clone(), self.channel)
    }

    fn failed(mut self, reason: daemon_core::payload::ChannelError) {
        self.told = true;
        self.core.closed(self.channel, reason);
    }
}

impl Drop for Telling {
    fn drop(&mut self) {
        if !self.told {
            self.core.closed(
                self.channel,
                daemon_core::payload::ChannelError::Handshake("the open was abandoned".to_owned()),
            );
        }
    }
}

/// A bulk transfer held after its first data arrives, so a test can show
/// what else moves while one is in flight. Dropping it releases the hold.
pub struct BulkResponseHold {
    entered: oneshot::Receiver<()>,
    release: Option<oneshot::Sender<()>>,
}

impl BulkResponseHold {
    /// Resolves once the held transfer's first data has arrived.
    pub async fn entered(&mut self) -> Result<(), oneshot::error::RecvError> {
        (&mut self.entered).await
    }

    pub fn release(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

impl Drop for BulkResponseHold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

struct PendingBulkResponseHold {
    entered: Option<oneshot::Sender<()>>,
    release: oneshot::Receiver<()>,
}

impl PendingBulkResponseHold {
    fn has_entered(&self) -> bool {
        self.entered.is_none()
    }

    fn enter(&mut self) {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
    }

    fn poll_release(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.release).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl ChannelPool {
    pub(crate) fn new(links: Arc<LinkRegistry>) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            opening: std::sync::Mutex::new(HashMap::new()),
            lifetimes: RwLock::new(HashMap::new()),
            links,
            security: None,
            handshake_timeout: CHANNEL_TLS_HANDSHAKE_TIMEOUT,
            bulk_response_holds: std::sync::Mutex::new(HashMap::new()),
            refused: std::sync::Mutex::default(),
        }
    }

    pub(crate) fn with_device_tls(
        links: Arc<LinkRegistry>,
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
    ) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            opening: std::sync::Mutex::new(HashMap::new()),
            lifetimes: RwLock::new(HashMap::new()),
            links,
            security: Some(ChannelSecurity {
                identity,
                trust_store,
            }),
            handshake_timeout: CHANNEL_TLS_HANDSHAKE_TIMEOUT,
            bulk_response_holds: std::sync::Mutex::new(HashMap::new()),
            refused: std::sync::Mutex::default(),
        }
    }

    /// From now on every channel to `peer` is refused while `refuse`
    /// holds, as a far end refusing every stream would refuse it, those
    /// already opening included, and no cached one is handed out.
    pub(crate) fn refuse(&self, peer: HostId, refuse: bool) {
        let mut refused = self
            .refused
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if refuse {
            refused.insert(peer);
        } else {
            refused.remove(&peer);
        }
    }

    fn refuses(&self, peer: HostId) -> bool {
        self.refused
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&peer)
    }

    /// Holds the next bulk channel opened to `peer` after the first data
    /// frame of its response.
    pub fn hold_next_bulk_response(&self, peer: HostId) -> BulkResponseHold {
        let (entered_tx, entered) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let previous = self
            .bulk_response_holds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                peer,
                PendingBulkResponseHold {
                    entered: Some(entered_tx),
                    release: release_rx,
                },
            );
        assert!(previous.is_none(), "a bulk response hold is already armed");
        BulkResponseHold {
            entered,
            release: Some(release),
        }
    }

    pub fn link_registry(&self) -> Arc<LinkRegistry> {
        self.links.clone()
    }

    pub(crate) async fn channel(&self, key: ChannelKey) -> Result<Channel, ChannelError> {
        if !key.class.shared() {
            return self.open_channel(key).await;
        }
        let opener = self
            .opening
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key)
            .or_default()
            .clone();
        let _opening = opener.lock().await;
        self.open_channel(key).await
    }

    async fn open_channel(&self, key: ChannelKey) -> Result<Channel, ChannelError> {
        let refused = self.refuses(key.peer);
        let cached = if key.class.shared() && !refused {
            self.by_key
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&key)
                .cloned()
        } else {
            None
        };
        if let Some(cached) = cached {
            if !self.route_is_live(key.route).await {
                return Err(ChannelError::LinkUnavailable {
                    host_id: route_link_peer(key.route),
                });
            }
            if !cached.ended.has_ended() {
                return Ok(cached.channel);
            }
            // Forget the ended channel, unless another call already put a
            // fresh one in its place.
            let mut by_key = self
                .by_key
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if by_key
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(&current.ended, &cached.ended))
            {
                by_key.remove(&key);
            }
        }

        let opened = std::time::Instant::now();
        let (link, carrier) = self.carrier(key.route).await?;
        let pinned = self.security.as_ref().and_then(|security| {
            security
                .trust_store
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pubkey_for_host(key.peer)
                .map(<[u8]>::to_vec)
        });
        // A profile's channels open at its core's word, with the key it
        // gives; a pool with no core pins from the trust store itself.
        let (telling, pin) = match self.links.core() {
            Some(core) => match core.want_channel(link, key.peer, key.class, pinned).await {
                Ok(opening) => {
                    let telling = Telling {
                        core: core.clone(),
                        channel: opening.channel,
                        told: false,
                    };
                    (Some(telling), Some(opening.pin.0))
                }
                Err(refusal) => {
                    return Err(match refusal {
                        daemon_core::payload::ChannelError::Unpinned => unpinned(key.peer),
                        _ => ChannelError::LinkUnavailable {
                            host_id: route_link_peer(key.route),
                        },
                    });
                }
            },
            None => (None, pinned),
        };
        if refused {
            let refusal = ChannelError::Refused(pb::StreamRefusal::NoRoute);
            if let Some(telling) = telling {
                telling.failed(core_error(&refusal));
            }
            return Err(refusal);
        }
        let ended = Arc::new(Ending::default());
        let secured = async {
            let (stream, handshake) = self
                .open_stream_on(carrier, key.peer, key.route, Handshake::Any)
                .await?;
            self.secure_channel(
                key,
                Watched::new(stream, ended.clone()),
                &ended,
                handshake,
                pin,
            )
            .await
            .map(|channel| (channel, handshake))
        }
        .await;
        // A refusal set while the channel was opening holds for it too, so
        // no channel opens once it is set.
        let secured = match secured {
            Ok(_) if self.refuses(key.peer) => {
                Err(ChannelError::Refused(pb::StreamRefusal::NoRoute))
            }
            secured => secured,
        };
        if let Some(telling) = telling {
            match &secured {
                Ok(_) => {
                    let (core, channel) = telling.opened(link, key);
                    ended.arm(core, channel);
                }
                Err(error) => telling.failed(core_error(error)),
            }
        }
        let (channel, handshake) = secured?;
        tracing::debug!(
            peer = %key.peer,
            class = ?key.class,
            ?handshake,
            elapsed_ms = opened.elapsed().as_millis(),
            "opened a link stream"
        );
        if key.class.shared() {
            self.by_key
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    key,
                    Cached {
                        channel: channel.clone(),
                        ended,
                    },
                );
        }
        Ok(channel)
    }

    pub(crate) async fn pairing_stream(
        &self,
        peer: HostId,
        route: Route,
    ) -> Result<ByteStream, ChannelError> {
        let (_, carrier) = self.carrier(route).await?;
        let (stream, _) = self
            .open_stream_on(carrier, peer, route, Handshake::Pinned)
            .await?;
        Ok(stream)
    }

    pub(crate) fn drop_host(&self, host: HostId) {
        self.forget_where(|key| key.peer == host || route_link_peer(key.route) == host);
    }

    pub(crate) fn drop_route(&self, peer: HostId, route: Route) {
        self.forget_where(|key| key.peer == peer && key.route == route);
    }

    /// Forgets the channels whose key `gone` names and cancels what ran on
    /// them. The opener locks go too: a key holds its link's id, fresh on
    /// every reconnect, so a daemon that kept them would hold one per
    /// link it ever had. A call still waiting on one holds its own clone
    /// and finishes as before.
    fn forget_where(&self, gone: impl Fn(&ChannelKey) -> bool) {
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| !gone(key));
        self.opening
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key, _| !gone(key));
        self.cancel_where(gone);
    }

    /// Opens a stream to `peer` on `route`, saying how it is secured. A
    /// stream to a paired host on a direct QUIC link of our own is plain:
    /// the link authenticated both ends when it came up, so the stream
    /// carries no handshake. Every other stream handshakes inside, and
    /// does not wait to be accepted first either: the handshake's first
    /// flight leaves with the preface, and a refusal comes back out of
    /// the handshake with its reason. Only an opener that asks for the
    /// pinned handshake outright, pairing, waits for the answer first.
    async fn open_stream_on(
        &self,
        carrier: Arc<dyn LinkCarrier>,
        peer: HostId,
        route: Route,
        wanted: Handshake,
    ) -> Result<(ByteStream, Handshake), ChannelError> {
        let plain = wanted == Handshake::Any
            && matches!(route, Route::Direct(link) if link.peer() == peer)
            && carrier.kind() == CarrierKind::Quic;
        let mut stream = carrier
            .open_stream(pb::StreamPreface {
                dst: peer.as_bytes().to_vec(),
                plain,
            })
            .await
            .map_err(|error| map_open_error(error, route))?;
        if plain {
            return Ok((stream, Handshake::None));
        }
        if wanted == Handshake::Pinned {
            stream
                .accepted()
                .await
                .map_err(|error| map_open_error(error, route))?;
        }
        Ok((stream, Handshake::Pinned))
    }

    /// The link `route` rides and its carrier, while it is up.
    async fn carrier(&self, route: Route) -> Result<(LinkId, Arc<dyn LinkCarrier>), ChannelError> {
        match route {
            Route::Direct(link) => self
                .links
                .native_carrier(&link)
                .await
                .map(|carrier| (link, carrier))
                .ok_or(ChannelError::LinkUnavailable {
                    host_id: link.peer(),
                }),
            Route::Via(relay) => self
                .links
                .native_carrier_to_peer(relay)
                .await
                .ok_or(ChannelError::LinkUnavailable { host_id: relay }),
        }
    }

    async fn secure_channel(
        &self,
        key: ChannelKey,
        stream: Watched<ByteStream>,
        ended: &Arc<Ending>,
        handshake: Handshake,
        pin: Option<Vec<u8>>,
    ) -> Result<Channel, ChannelError> {
        let security = self
            .security
            .as_ref()
            .ok_or_else(|| ChannelError::Handshake("device identity is unavailable".to_string()))?;
        let lifetime = Arc::new(CancellationToken::new());
        {
            let mut lifetimes = self
                .lifetimes
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tracked = lifetimes.entry(key).or_default();
            tracked.retain(|lifetime| lifetime.strong_count() > 0);
            tracked.push(Arc::downgrade(&lifetime));
        }
        let stream = crate::transport::ShutdownIo::new_shared(stream, lifetime);
        let io: Box<dyn crate::transport::BoxedGrpcInner> = match handshake {
            Handshake::None => Box::new(stream),
            Handshake::Pinned | Handshake::Any => {
                let pin = pin.ok_or_else(|| unpinned(key.peer))?;
                let config = security.identity.client_tls_config_pinned(key.peer, pin)?;
                let connector = TlsConnector::from(Arc::new(config));
                let server_name = ServerName::try_from("amux-device".to_string())
                    .map_err(|error| ChannelError::Tls(error.to_string()))?;
                let tls = tokio::time::timeout(
                    self.handshake_timeout,
                    connector.connect(server_name, stream),
                )
                .await
                .map_err(|_| ChannelError::Handshake("TLS handshake timed out".to_string()))?
                .map_err(|error| match OpenError::carried(&error) {
                    // The stream was not waited on: the open's answer is
                    // read here, under the handshake, and is the error.
                    Some(OpenError::Refused(reason)) => ChannelError::Refused(*reason),
                    Some(OpenError::LinkClosed) => ChannelError::LinkUnavailable {
                        host_id: route_link_peer(key.route),
                    },
                    _ => ChannelError::Tls(error.to_string()),
                })?;
                Box::new(tls)
            }
        };
        // Compressed above the handshake, so the relay copies it as it
        // copies any ciphertext; the acceptor wraps its end the same way
        // (`TunnelDispatcher`).
        let tls = Compressed::new(TrustAnswer::new(io, key.peer, ended.clone()));
        let endpoint = configure_tonic_endpoint_keepalive(Endpoint::from_static("https://peer"));
        let hold = (key.class == ChannelClass::Bulk).then(|| {
            self.bulk_response_holds
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&key.peer)
        });
        Ok(match hold.flatten() {
            Some(hold) => channel_from_single_io(
                endpoint,
                "held native link channel",
                HoldAfterFirstDataFrame::new(tls, hold),
            ),
            None => channel_from_single_io(endpoint, "native link channel", tls),
        })
    }

    fn cancel_where(&self, predicate: impl Fn(&ChannelKey) -> bool) {
        let mut lifetimes = self
            .lifetimes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let keys = lifetimes
            .keys()
            .copied()
            .filter(|key| predicate(key))
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(tokens) = lifetimes.remove(&key) {
                for token in tokens {
                    if let Some(token) = token.upgrade() {
                        token.cancel();
                    }
                }
            }
        }
    }

    async fn route_is_live(&self, route: Route) -> bool {
        match route {
            Route::Direct(link) => self.links.native_carrier(&link).await.is_some(),
            Route::Via(relay) => self.links.native_carrier_to_peer(relay).await.is_some(),
        }
    }
}

/// A stream that says when it has ended: at end of file, at an error, or
/// when the connection over it lets it go.
struct Watched<T> {
    inner: T,
    ended: Arc<Ending>,
}

impl<T> Watched<T> {
    fn new(inner: T, ended: Arc<Ending>) -> Self {
        Self { inner, ended }
    }

    fn end_on<R>(&self, result: &Poll<io::Result<R>>) {
        if matches!(result, Poll::Ready(Err(_))) {
            self.ended.end();
        }
    }
}

impl<T> Drop for Watched<T> {
    fn drop(&mut self) {
        self.ended.end();
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Watched<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let (before, room) = (buf.filled().len(), buf.remaining() > 0);
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if room && matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() == before {
            self.ended.end();
        }
        self.end_on(&result);
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Watched<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.end_on(&result);
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.end_on(&result);
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.end_on(&result);
        result
    }
}

/// The message a call carries when the host it called refused this host's
/// certificate as no longer trusted.
pub const TRUST_REVOKED: &str = "the host no longer trusts this machine";

/// Reads the far host's answer to this host's certificate off a channel's
/// TLS stream. In TLS 1.3 the client's handshake finishes before the server
/// has judged the client's certificate, so the answer comes with the first
/// read: application data means it was taken, and a certificate_revoked
/// alert means the host no longer trusts this one. Either is told to the
/// profile's core, once the channel is reported open, and the refusal fails
/// the read with an error [`PeerChannel`] names to the caller.
struct TrustAnswer<T> {
    inner: T,
    peer: HostId,
    channel: Arc<Ending>,
    answered: bool,
}

impl<T> TrustAnswer<T> {
    fn new(inner: T, peer: HostId, channel: Arc<Ending>) -> Self {
        Self {
            inner,
            peer,
            channel,
            answered: false,
        }
    }
}

fn revoked_certificate(error: &io::Error) -> bool {
    matches!(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rustls::Error>()),
        Some(rustls::Error::AlertReceived(
            rustls::AlertDescription::CertificateRevoked
        ))
    )
}

impl<T: AsyncRead + Unpin> AsyncRead for TrustAnswer<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match ready!(Pin::new(&mut self.inner).poll_read(cx, buf)) {
            Ok(()) => {
                if !self.answered && buf.filled().len() > before {
                    self.answered = true;
                    self.channel.tell_answered();
                }
                Poll::Ready(Ok(()))
            }
            Err(error) if !self.answered && revoked_certificate(&error) => {
                self.answered = true;
                self.channel.tell_revoked(self.peer);
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    TRUST_REVOKED,
                )))
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TrustAnswer<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// A channel to a trusted host whose calls fail with an Unauthenticated
/// status saying so when that host no longer trusts this one, rather than
/// with the anonymous transport error the refused stream would otherwise
/// read as.
#[derive(Clone)]
pub struct PeerChannel(Channel);

impl PeerChannel {
    pub(crate) fn new(channel: Channel) -> Self {
        Self(channel)
    }
}

impl tower::Service<http::Request<tonic::body::Body>> for PeerChannel {
    type Response = http::Response<tonic::body::Body>;
    type Error = tower::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx).map_err(name_refusal)
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        let response = self.0.call(request);
        Box::pin(async move { response.await.map_err(name_refusal) })
    }
}

/// A PeerService client on a [`PeerChannel`].
pub type PeerClient = wire::peer_service_client::PeerServiceClient<PeerChannel>;

pub(crate) fn peer_client(channel: Channel) -> PeerClient {
    wire::peer_service_client::PeerServiceClient::new(PeerChannel::new(channel))
        .max_decoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
}

/// The status for a transport error: Unauthenticated when [`TrustAnswer`]
/// failed the stream because the host refused this one's certificate.
/// HTTP/2 keeps only an I/O error's kind and text, so those are what is
/// matched.
fn name_refusal(error: tonic::transport::Error) -> tower::BoxError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<io::Error>() {
            if io.kind() == io::ErrorKind::PermissionDenied && io.to_string() == TRUST_REVOKED {
                return Box::new(tonic::Status::unauthenticated(TRUST_REVOKED));
            }
            // A plain stream is not waited on, so the host's refusal of it
            // is read, not answered at the open.
            if let Some(reason) = OpenError::refusal(io) {
                return Box::new(tonic::Status::unavailable(format!(
                    "stream refused: {reason:?}"
                )));
            }
        }
        source = current.source();
    }
    Box::new(error)
}

enum Http2ReadState {
    Header {
        bytes: [u8; HTTP2_FRAME_HEADER_LEN],
        filled: usize,
    },
    Payload {
        frame_type: u8,
        remaining: usize,
    },
}

struct HoldAfterFirstDataFrame<T> {
    inner: T,
    hold: Option<PendingBulkResponseHold>,
    state: Http2ReadState,
}

impl<T> HoldAfterFirstDataFrame<T> {
    fn new(inner: T, hold: PendingBulkResponseHold) -> Self {
        Self {
            inner,
            hold: Some(hold),
            state: Http2ReadState::Header {
                bytes: [0; HTTP2_FRAME_HEADER_LEN],
                filled: 0,
            },
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for HoldAfterFirstDataFrame<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.hold.as_ref().is_some_and(|hold| hold.has_entered()) {
            ready!(this.hold.as_mut().expect("hold exists").poll_release(cx));
            this.hold = None;
        }
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let boundary = match &this.state {
            Http2ReadState::Header { filled, .. } => HTTP2_FRAME_HEADER_LEN - filled,
            Http2ReadState::Payload { remaining, .. } => *remaining,
        };
        let before = output.filled().len();
        let (poll, initialized, read) = {
            let mut limited = output.take(boundary);
            let poll = Pin::new(&mut this.inner).poll_read(cx, &mut limited);
            (poll, limited.initialized().len(), limited.filled().len())
        };
        // SAFETY: `limited` covered exactly this prefix of `output`'s
        // unfilled storage and reported it initialized by the inner reader.
        unsafe { output.assume_init(initialized) };
        output.advance(read);
        match poll {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        if read == 0 {
            return Poll::Ready(Ok(()));
        }
        let bytes = &output.filled()[before..before + read];
        match &mut this.state {
            Http2ReadState::Header {
                bytes: header,
                filled,
            } => {
                header[*filled..*filled + read].copy_from_slice(bytes);
                *filled += read;
                if *filled == HTTP2_FRAME_HEADER_LEN {
                    let length = usize::from(header[0]) << 16
                        | usize::from(header[1]) << 8
                        | usize::from(header[2]);
                    if length == 0 {
                        this.state = Http2ReadState::Header {
                            bytes: [0; HTTP2_FRAME_HEADER_LEN],
                            filled: 0,
                        };
                    } else {
                        this.state = Http2ReadState::Payload {
                            frame_type: header[3],
                            remaining: length,
                        };
                    }
                }
            }
            Http2ReadState::Payload {
                frame_type,
                remaining,
            } => {
                *remaining -= read;
                if *remaining == 0 {
                    let is_data = *frame_type == HTTP2_DATA_FRAME;
                    this.state = Http2ReadState::Header {
                        bytes: [0; HTTP2_FRAME_HEADER_LEN],
                        filled: 0,
                    };
                    if is_data && let Some(hold) = &mut this.hold {
                        hold.enter();
                    }
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for HoldAfterFirstDataFrame<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// A stream a peer opened to this host, from the link it arrived on.
pub(crate) struct InboundStream {
    /// The host at the far end of that link, authenticated by it.
    pub(crate) adjacent_peer: HostId,
    pub(crate) stream: ByteStream,
    /// The stream is the adjacent peer's own, over a direct link, and
    /// carries no handshake: the link's authentication is the stream's.
    pub(crate) plain: bool,
}

pub(crate) fn serve_inbound_streams(
    dispatcher: Arc<TunnelDispatcher>,
    mut streams: mpsc::Receiver<InboundStream>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(inbound) = streams.recv().await {
            let dispatcher = dispatcher.clone();
            tokio::spawn(async move {
                if let Err(error) = dispatcher.dispatch_link_stream(inbound).await {
                    if error.is_peer_leaving() {
                        // The peer opened this and left before it said who it
                        // was, which is what a stream on a superseded link
                        // does. Nothing here was refused.
                        tracing::debug!(%error, "a peer left a native link stream unfinished");
                    } else {
                        tracing::warn!(%error, "dispatcher rejected native link stream");
                    }
                }
            });
        }
    })
}

/// A channel to a host no key is pinned for: not paired, or its pairing is
/// being replaced.
fn unpinned(peer: HostId) -> ChannelError {
    ChannelError::Tls(format!("no key is pinned for host {peer}"))
}

fn map_open_error(error: OpenError, route: Route) -> ChannelError {
    match error {
        OpenError::Refused(reason) => ChannelError::Refused(reason),
        OpenError::LinkClosed => ChannelError::LinkUnavailable {
            host_id: route_link_peer(route),
        },
        OpenError::Io(error) => ChannelError::Handshake(error.to_string()),
    }
}

pub(crate) fn route_link_peer(route: Route) -> HostId {
    match route {
        Route::Direct(link) => link.peer(),
        Route::Via(relay) => relay,
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn opener_count(pool: &ChannelPool) -> usize {
        pool.opening.lock().unwrap().len()
    }

    fn opened(pool: &ChannelPool, key: ChannelKey) {
        pool.opening.lock().unwrap().entry(key).or_default();
    }

    #[test]
    fn dropping_a_link_a_route_or_a_host_forgets_their_opener_locks() {
        let pool = ChannelPool::new(Arc::new(LinkRegistry::new(crate::HostId::nil())));
        let host = Uuid::new_v4();
        let relay = Uuid::new_v4();
        let other = Uuid::new_v4();
        let link = LinkId::new(host);
        let direct = ChannelKey {
            peer: host,
            route: Route::Direct(link),
            class: ChannelClass::Session,
        };
        let via = ChannelKey {
            peer: host,
            route: Route::Via(relay),
            class: ChannelClass::Calls,
        };
        let elsewhere = ChannelKey {
            peer: other,
            route: Route::Direct(LinkId::new(other)),
            class: ChannelClass::Calls,
        };
        opened(&pool, direct);
        opened(&pool, via);
        opened(&pool, elsewhere);
        assert_eq!(opener_count(&pool), 3);

        pool.drop_route(host, Route::Direct(link));
        assert_eq!(opener_count(&pool), 2, "the link's lock went with the link");

        pool.drop_route(host, Route::Via(relay));
        assert_eq!(
            opener_count(&pool),
            1,
            "the route's lock went with the route"
        );

        pool.drop_host(other);
        assert_eq!(opener_count(&pool), 0, "the host's lock went with the host");
    }
}
