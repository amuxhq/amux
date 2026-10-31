//! Tonic channels carried by independent native link streams.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock, Weak};
use std::task::{Context, Poll, ready};

use futures_util::future::BoxFuture;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tonic::codegen::http;
use tonic::transport::{Channel, Endpoint};
use wire::pb;

use super::{ByteStream, OpenError, QuicCarrier};
use crate::HostId;
use crate::dispatcher::TunnelDispatcher;
use crate::driver::{CoreLinks, core_error};
use crate::identity::IdentityError;
use crate::routing::{LinkId, LinkRegistry, Route};
use crate::transport::{Compressed, channel_from_single_io};
use crate::trust::SharedTrustStore;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelClass {
    Calls,
    /// Session subscriptions, on a channel of their own so that they
    /// neither wait behind nor hold up the host's calls.
    Session,
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
        "Pairing could not reach this host. Check that both devices are online, and on the same network, signed in to the same account or paired with the same host."
    )]
    NoPairingRoute,
    #[error("the forwarder refused to carry the link: {0:?}")]
    Refused(pb::ForwardRefusal),
    #[error("no live link to host {host_id}")]
    LinkUnavailable { host_id: HostId },
    #[error("channel handshake failed: {0}")]
    Handshake(String),
    /// What answered on one path ended the dial before the others could.
    #[error("channel handshake failed: {error}")]
    TurnedAway {
        path: super::socket::Path,
        error: String,
    },
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("channel TLS failed: {0}")]
    Tls(String),
    /// The far host refused this host's certificate in the link's
    /// handshake: it no longer trusts this one.
    #[error("{TRUST_REVOKED}")]
    Revoked,
}

/// What a call to a host says when that host refused this host's
/// certificate as no longer trusted: the words the core keeps as the
/// host's dial error too.
pub use daemon_core::payload::TRUST_REVOKED;

/// Whether `status` is a host saying it no longer trusts this one, as
/// [`ChannelError::status`] writes it.
pub(crate) fn says_revoked(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::Unauthenticated && status.message() == TRUST_REVOKED
}

impl ChannelError {
    /// The status a call that found no channel fails with: one refused as
    /// no longer trusted is `UNAUTHENTICATED`, naming that; any other is
    /// `UNAVAILABLE`.
    pub fn status(&self) -> tonic::Status {
        match self {
            ChannelError::Revoked => tonic::Status::unauthenticated(TRUST_REVOKED),
            _ => tonic::Status::unavailable(self.to_string()),
        }
    }
}

/// What a call reaches a host it has no link to by, and a pairing a host
/// through a forwarder by.
pub(crate) trait ForwardedDial: Send + Sync + 'static {
    /// Asks for a link of `peer`'s own now, returning once the attempt it
    /// brought about has ended, however it ended.
    fn want(&self, peer: HostId) -> BoxFuture<'_, Result<(), ChannelError>>;

    /// Dials `peer`'s pairing service through the forwarder at the far end
    /// of `through`, presenting no certificate: the host answers only while
    /// its pairing window is open. The dial is given up after `deadline`.
    fn pair_through(
        &self,
        peer: HostId,
        through: LinkId,
        deadline: std::time::Duration,
    ) -> BoxFuture<'_, Result<Channel, ChannelError>>;
}

/// Where a session subscription starts: the origin's tail, or after a
/// revision this host already holds.
pub type Cursor = wire::peer_subscribe_session_request::From;

/// The only calls the channel pool writes before a link's handshake
/// finishes, as early data the far end may see more than once: each reads
/// and changes nothing. A send, a stop or a pairing call has no variant
/// here, so it cannot be early data.
///
/// ```compile_fail
/// let _ = node::EarlyCall::SendInput(wire::SendInputRequest::default());
/// ```
///
/// ```
/// let _ = node::EarlyCall::ListItems(wire::ListItemsRequest::default());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum EarlyCall {
    /// The host's inventory, which always opens with all of it: the wire
    /// gives it no cursor.
    SubscribeInventory,
    SubscribeSession {
        session: Vec<u8>,
        from: Cursor,
    },
    ListItems(wire::ListItemsRequest),
    GetBlob(wire::GetBlobRequest),
    GetDiff(wire::GetDiffRequest),
}

impl EarlyCall {
    /// The channel the call rides, as the same call made later would.
    fn class(&self) -> ChannelClass {
        match self {
            EarlyCall::SubscribeInventory
            | EarlyCall::ListItems(_)
            | EarlyCall::GetBlob(_)
            | EarlyCall::GetDiff(_) => ChannelClass::Calls,
            EarlyCall::SubscribeSession { .. } => ChannelClass::Session,
        }
    }

    /// The service method the call is.
    fn method(&self) -> &'static str {
        match self {
            EarlyCall::SubscribeInventory => "SubscribeInventory",
            EarlyCall::SubscribeSession { .. } => "SubscribeSession",
            EarlyCall::ListItems(_) => "ListItems",
            EarlyCall::GetBlob(_) => "GetBlob",
            EarlyCall::GetDiff(_) => "GetDiff",
        }
    }

    async fn make(self, mut client: PeerClient) -> Result<EarlyAnswer, tonic::Status> {
        Ok(match self {
            EarlyCall::SubscribeInventory => EarlyAnswer::Inventory(
                client
                    .agents
                    .subscribe_inventory(wire::SubscribeInventoryRequest {})
                    .await?
                    .into_inner(),
            ),
            EarlyCall::SubscribeSession { session, from } => EarlyAnswer::Session(
                client
                    .sessions
                    .subscribe_session(wire::PeerSubscribeSessionRequest {
                        agent_id: session,
                        from: Some(from),
                    })
                    .await?
                    .into_inner(),
            ),
            EarlyCall::ListItems(request) => {
                EarlyAnswer::Fetched(client.sessions.list_items(request).await?.into_inner())
            }
            EarlyCall::GetBlob(request) => {
                EarlyAnswer::Blob(client.agents.get_blob(request).await?.into_inner())
            }
            EarlyCall::GetDiff(request) => {
                EarlyAnswer::Diff(client.agents.get_diff(request).await?.into_inner())
            }
        })
    }
}

/// The answer to an early call: the stream a subscription opened, or a
/// unary call's reply.
#[derive(Debug)]
pub enum EarlyAnswer {
    Inventory(tonic::Streaming<wire::InventoryEvent>),
    Session(tonic::Streaming<wire::SessionEvent>),
    Fetched(wire::ListItemsResponse),
    Blob(wire::GetBlobResponse),
    Diff(wire::Diff),
}

/// An early call under way: it went out with the link's first packets, and
/// resolves to its answer once the host has given one.
pub struct EarlyReply(JoinHandle<Result<EarlyAnswer, tonic::Status>>);

impl Future for EarlyReply {
    type Output = Result<EarlyAnswer, tonic::Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(match ready!(Pin::new(&mut self.0).poll(cx)) {
            Ok(answer) => answer,
            Err(error) => Err(tonic::Status::cancelled(error.to_string())),
        })
    }
}

/// A link a resumed dial brought up, and the calls written on it before
/// its handshake finished, in the order they were given.
pub struct ResumedLink {
    pub link: LinkId,
    pub early_data: super::link_log::EarlyData,
    pub replies: Vec<EarlyReply>,
    /// What carries the link, which names the path it came up on.
    pub(crate) carrier: Arc<QuicCarrier>,
}

/// A link a resumed dial has started: its carrier already carries data,
/// and its connector has written the Hello.
pub(crate) struct Resuming {
    pub(crate) carrier: Arc<QuicCarrier>,
    /// Resolves with the link once the host has answered the Hello and the
    /// link is registered, or with why it did not come up.
    pub(crate) established: BoxFuture<'static, Result<LinkId, ChannelError>>,
}

/// The paths one dial's first flight goes out on at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flight {
    pub(crate) paths: Vec<super::socket::Path>,
    /// Every link to the relay too, those that come up during the flight
    /// as well.
    pub(crate) through_relay: bool,
}

impl std::fmt::Display for Flight {
    /// Where the flight went, as a dial's error names it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut tried: Vec<String> = self
            .paths
            .iter()
            .map(|path| match path {
                super::socket::Path::Direct(addr) => addr.to_string(),
                super::socket::Path::Forwarded(link) => format!("through link {}", link.0),
                path => format!("{path:?}"),
            })
            .collect();
        if self.through_relay {
            tried.push("through the relay".to_owned());
        }
        f.write_str(&tried.join(", "))
    }
}

/// Dials a host resuming with the ticket its last link left.
pub(crate) trait ResumedDial: Send + Sync + 'static {
    /// Starts a link to `peer` on every path of `flight` and its
    /// connector, returning as soon as the link can carry data: where the
    /// dial holds a ticket, before the handshake finishes, so what is
    /// written next goes out as early data.
    fn dial_resumed(
        &self,
        peer: HostId,
        flight: Flight,
    ) -> BoxFuture<'_, Result<Resuming, ChannelError>>;
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
    /// The link the channel rides. A route through a forwarder outlives
    /// the link of the peer's own it was carried on, and a channel on a
    /// link going is not yet ended while its connection lingers.
    link: LinkId,
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
}

pub struct ChannelPool {
    by_key: RwLock<HashMap<ChannelKey, Cached>>,
    /// One opener per shared channel: callers who ask while it is being
    /// opened wait for it rather than opening their own, which at a launch
    /// every agent's source would otherwise do at once.
    opening: std::sync::Mutex<HashMap<ChannelKey, Arc<tokio::sync::Mutex<()>>>>,
    lifetimes: RwLock<HashMap<ChannelKey, Vec<Weak<CancellationToken>>>>,
    links: Arc<LinkRegistry>,
    /// Where the key the core opens a channel under is pinned.
    trust_store: SharedTrustStore,
    /// How a link through a forwarder is dialled, once the edge is up.
    forwarded: OnceLock<Arc<dyn ForwardedDial>>,
    /// How a link resuming with a ticket is dialled, once the edge is up.
    resumed: OnceLock<Arc<dyn ResumedDial>>,
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

impl ChannelPool {
    pub(crate) fn new(links: Arc<LinkRegistry>, trust_store: SharedTrustStore) -> Self {
        Self {
            by_key: RwLock::new(HashMap::new()),
            opening: std::sync::Mutex::new(HashMap::new()),
            lifetimes: RwLock::new(HashMap::new()),
            links,
            trust_store,
            forwarded: OnceLock::new(),
            resumed: OnceLock::new(),
            refused: std::sync::Mutex::default(),
        }
    }

    /// The key pinned for `peer`, which the core opens its channels under.
    fn pinned(&self, peer: HostId) -> Option<Vec<u8>> {
        self.trust_store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pubkey_for_host(peer)
            .map(<[u8]>::to_vec)
    }

    /// Dials links through forwarders with `dial` from now on.
    pub(crate) fn dial_forwarded_with(&self, dial: Arc<dyn ForwardedDial>) {
        let _ = self.forwarded.set(dial);
    }

    /// Dials links resuming with tickets with `dial` from now on.
    pub(crate) fn dial_resumed_with(&self, dial: Arc<dyn ResumedDial>) {
        let _ = self.resumed.set(dial);
    }

    /// Brings up a link to `peer` resuming with the ticket its last link
    /// left, writing the link's Hello and then `early` before the handshake
    /// finishes, each on the channel the same call would ride later. If the
    /// host refuses the early data, the same calls go out again once the
    /// handshake has finished; with no ticket they follow the handshake.
    /// The channels stay the pool's, for the calls that come after.
    pub(crate) async fn open_resumed_on(
        &self,
        peer: HostId,
        early: Vec<EarlyCall>,
        flight: Flight,
    ) -> Result<ResumedLink, ChannelError> {
        let dial = self
            .resumed
            .get()
            .ok_or(ChannelError::LinkUnavailable { host_id: peer })?;
        let Resuming {
            carrier,
            established,
        } = dial.dial_resumed(peer, flight).await?;
        // One stream for each channel the calls ride, opened and written at
        // once: the carrier sends both as early data where it can.
        let mut opened: Vec<(ChannelClass, Channel, Arc<Ending>, Arc<CancellationToken>)> =
            Vec::new();
        let mut replies = Vec::with_capacity(early.len());
        for call in early {
            let class = call.class();
            let channel = match opened.iter().find(|(opened, ..)| *opened == class) {
                Some((_, channel, ..)) => channel.clone(),
                None => {
                    let stream = match carrier.open_stream().await {
                        Ok(stream) => stream,
                        // The link ended before its early calls went out:
                        // why it ended (a host refusing the ticket, say) is
                        // the dial's answer.
                        Err(error) => {
                            return Err(match established.await {
                                Err(ended) => ended,
                                Ok(_) => map_open_error(error, Route::Via(peer)),
                            });
                        }
                    };
                    tracing::debug!(%peer, ?class, "opened an early link stream");
                    let ended = Arc::new(Ending::default());
                    let lifetime = Arc::new(CancellationToken::new());
                    let channel = channel_on(
                        Watched::new(stream, ended.clone()),
                        &ended,
                        lifetime.clone(),
                    );
                    opened.push((class, channel.clone(), ended, lifetime));
                    channel
                }
            };
            tracing::debug!(%peer, method = call.method(), "writing an early call");
            replies.push(EarlyReply(tokio::spawn(call.make(peer_client(channel)))));
        }
        let link = match established.await {
            Ok(link) => link,
            Err(error) => {
                for (.., lifetime) in &opened {
                    lifetime.cancel();
                }
                return Err(error);
            }
        };
        for (class, channel, ended, lifetime) in opened {
            self.adopt(
                ChannelKey {
                    peer,
                    route: Route::Direct(link),
                    class,
                },
                link,
                channel,
                ended,
                lifetime,
            )
            .await;
        }
        Ok(ResumedLink {
            link,
            early_data: carrier.early_data(),
            replies,
            carrier,
        })
    }

    /// Takes a channel opened on `link` before the link was registered into
    /// the pool: the core hears of it as of any channel it opened, and a
    /// shared one serves the calls that come after. One the core will not
    /// have, to a host no key is pinned for, is let go.
    async fn adopt(
        &self,
        key: ChannelKey,
        link: LinkId,
        channel: Channel,
        ended: Arc<Ending>,
        lifetime: Arc<CancellationToken>,
    ) {
        if let Some(core) = self.links.core() {
            let pinned = self.pinned(key.peer);
            match core.want_channel(link, key.peer, key.class, pinned).await {
                Ok(opening) => {
                    let telling = Telling {
                        core: core.clone(),
                        channel: opening.channel,
                        told: false,
                    };
                    let (core, channel) = telling.opened(link, key);
                    ended.arm(core, channel);
                }
                Err(_) => {
                    lifetime.cancel();
                    return;
                }
            }
        }
        self.track(key, &lifetime);
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key,
                Cached {
                    channel,
                    ended,
                    link,
                },
            );
    }

    /// From now on every channel to `peer` is refused while `refuse`
    /// holds, as a link whose far end takes no stream would refuse it,
    /// those already opening included, and no cached one is handed out.
    /// The link each refusal was on closes, as any link a stream would not
    /// open on does.
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

    /// A stream would not open on `link` while it stayed up: it closes as
    /// a dead connection would, so the host reads as the links that remain
    /// say, and redialling brings it back.
    fn close_failed(&self, link: LinkId) {
        tracing::warn!(?link, "no stream opened on the link; closing it");
        let links = self.links.clone();
        tokio::spawn(async move {
            links
                .close_link(link, crate::routing::LinkCloseRequest::CallsFailed)
                .await;
        });
    }

    pub fn link_registry(&self) -> Arc<LinkRegistry> {
        self.links.clone()
    }

    pub(crate) async fn channel(&self, key: ChannelKey) -> Result<Channel, ChannelError> {
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
        let cached = if !refused {
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
            if !cached.ended.has_ended() && self.links.native_carrier(&cached.link).await.is_some()
            {
                return Ok(cached.channel);
            }
            // Forget the ended channel, or the one whose link went, unless
            // another call already put a fresh one in its place.
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
        let (link, carrier) = self.carrier(key.peer, key.route).await?;
        let unavailable = || ChannelError::LinkUnavailable {
            host_id: route_link_peer(key.route),
        };
        // A channel opens at the profile core's word, which refuses a host
        // no key is pinned for.
        let core = self.links.core().ok_or_else(unavailable)?;
        let telling = match core
            .want_channel(link, key.peer, key.class, self.pinned(key.peer))
            .await
        {
            Ok(opening) => Telling {
                core: core.clone(),
                channel: opening.channel,
                told: false,
            },
            Err(daemon_core::payload::ChannelError::Unpinned) => return Err(unpinned(key.peer)),
            Err(_) => return Err(unavailable()),
        };
        if refused {
            let refusal = streams_refused();
            telling.failed(core_error(&refusal));
            self.close_failed(link);
            return Err(refusal);
        }
        let ended = Arc::new(Ending::default());
        let secured = async {
            let stream = carrier
                .open_stream()
                .await
                .map_err(|error| map_open_error(error, key.route))?;
            Ok(self.channel_over(key, Watched::new(stream, ended.clone()), &ended))
        }
        .await;
        // A refusal set while the channel was opening holds for it too, so
        // no channel opens once it is set.
        let secured = match secured {
            Ok(_) if self.refuses(key.peer) => Err(streams_refused()),
            secured => secured,
        };
        if let Err(ChannelError::Handshake(_)) = &secured {
            self.close_failed(link);
        }
        match &secured {
            Ok(_) => {
                let (core, channel) = telling.opened(link, key);
                ended.arm(core, channel);
            }
            Err(error) => telling.failed(core_error(error)),
        }
        let channel = secured?;
        tracing::debug!(
            peer = %key.peer,
            class = ?key.class,
            elapsed_ms = opened.elapsed().as_millis(),
            "opened a link stream"
        );
        self.by_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key,
                Cached {
                    channel: channel.clone(),
                    ended,
                    link,
                },
            );
        Ok(channel)
    }

    /// A channel to `peer`'s pairing service through `forwarder`, its dial
    /// given up after `deadline`.
    pub(crate) async fn pairing_channel(
        &self,
        peer: HostId,
        forwarder: HostId,
        deadline: std::time::Duration,
    ) -> Result<Channel, ChannelError> {
        let (through, _) = self
            .links
            .native_carrier_to_peer(forwarder)
            .await
            .ok_or(ChannelError::LinkUnavailable { host_id: forwarder })?;
        self.forwarded
            .get()
            .ok_or(ChannelError::LinkUnavailable { host_id: forwarder })?
            .pair_through(peer, through, deadline)
            .await
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

    /// The link a channel to `peer` on `route` rides and its carrier. A
    /// route through a forwarder rides a link of `peer`'s own, whose
    /// packets the forwarder carries: the one up, or one the core dials
    /// now.
    async fn carrier(
        &self,
        peer: HostId,
        route: Route,
    ) -> Result<(LinkId, Arc<QuicCarrier>), ChannelError> {
        match route {
            Route::Direct(link) => self
                .links
                .native_carrier(&link)
                .await
                .map(|carrier| (link, carrier))
                .ok_or(ChannelError::LinkUnavailable {
                    host_id: link.peer(),
                }),
            Route::Via(forwarder) => {
                if let Some(up) = self.links.native_carrier_to_peer(peer).await {
                    return Ok(up);
                }
                self.forwarded
                    .get()
                    .ok_or(ChannelError::LinkUnavailable { host_id: forwarder })?
                    .want(peer)
                    .await?;
                self.links
                    .native_carrier_to_peer(peer)
                    .await
                    .ok_or(ChannelError::LinkUnavailable { host_id: forwarder })
            }
        }
    }

    /// The channel over a stream of the link: its handshake authenticated
    /// both ends, so the stream carries the channel's bytes at once.
    fn channel_over(
        &self,
        key: ChannelKey,
        stream: Watched<ByteStream>,
        ended: &Arc<Ending>,
    ) -> Channel {
        let lifetime = Arc::new(CancellationToken::new());
        self.track(key, &lifetime);
        channel_on(stream, ended, lifetime)
    }

    /// Cancels `lifetime` when the channels `key` names are forgotten.
    fn track(&self, key: ChannelKey, lifetime: &Arc<CancellationToken>) {
        let mut lifetimes = self
            .lifetimes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tracked = lifetimes.entry(key).or_default();
        tracked.retain(|lifetime| lifetime.strong_count() > 0);
        tracked.push(Arc::downgrade(lifetime));
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

/// Tells the profile's core when the far host first answers on a channel:
/// its first bytes say the host took the link this channel rides.
struct TrustAnswer<T> {
    inner: T,
    channel: Arc<Ending>,
    answered: bool,
}

impl<T> TrustAnswer<T> {
    fn new(inner: T, channel: Arc<Ending>) -> Self {
        Self {
            inner,
            channel,
            answered: false,
        }
    }
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
        self.0.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        let response = self.0.call(request);
        Box::pin(async move { response.await.map_err(Into::into) })
    }
}

/// The peer services' clients on one [`PeerChannel`].
#[derive(Clone)]
pub struct PeerClient {
    pub agents: wire::peer_agent_service_client::PeerAgentServiceClient<PeerChannel>,
    pub sessions: wire::peer_session_service_client::PeerSessionServiceClient<PeerChannel>,
}

pub(crate) fn peer_client(channel: Channel) -> PeerClient {
    let channel = PeerChannel::new(channel);
    PeerClient {
        agents: wire::peer_agent_service_client::PeerAgentServiceClient::new(channel.clone())
            .max_decoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
            .max_encoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT),
        sessions: wire::peer_session_service_client::PeerSessionServiceClient::new(channel)
            .max_decoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
            .max_encoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT),
    }
}

/// A channel over `stream`, ending when `lifetime` is cancelled.
fn channel_on(
    stream: Watched<ByteStream>,
    ended: &Arc<Ending>,
    lifetime: Arc<CancellationToken>,
) -> Channel {
    let stream = crate::transport::ShutdownIo::new_shared(stream, lifetime);
    // The acceptor wraps its end the same way (`TunnelDispatcher`).
    let io = Compressed::new(TrustAnswer::new(stream, ended.clone()));
    channel_from_single_io(
        Endpoint::from_static("https://peer"),
        "native link channel",
        io,
    )
}

/// A stream a peer opened to this host, from the link it arrived on.
pub(crate) struct InboundStream {
    /// The host at the far end of that link, authenticated by it.
    pub(crate) adjacent_peer: HostId,
    pub(crate) stream: ByteStream,
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
                    tracing::warn!(%error, "dispatcher rejected native link stream");
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

/// What a channel the refusal hook turned away fails with: a stream that
/// would not open, as on a link whose far end takes none.
fn streams_refused() -> ChannelError {
    ChannelError::Handshake("the link refused the stream".to_owned())
}

fn map_open_error(error: OpenError, route: Route) -> ChannelError {
    match error {
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
        let pool = ChannelPool::new(
            Arc::new(LinkRegistry::new(crate::HostId::nil())),
            SharedTrustStore::default(),
        );
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

    /// The early-call type has a variant for each call that reads, and for
    /// nothing that writes: the match below has no wildcard, so a variant
    /// added to it fails to compile here until it is listed.
    #[test]
    fn an_early_call_is_only_a_call_that_changes_nothing() {
        let every = [
            EarlyCall::SubscribeInventory,
            EarlyCall::SubscribeSession {
                session: Vec::new(),
                from: Cursor::Tail(10),
            },
            EarlyCall::ListItems(wire::ListItemsRequest::default()),
            EarlyCall::GetBlob(wire::GetBlobRequest::default()),
            EarlyCall::GetDiff(wire::GetDiffRequest::default()),
        ];
        for call in &every {
            match call {
                EarlyCall::SubscribeInventory
                | EarlyCall::SubscribeSession { .. }
                | EarlyCall::ListItems(_)
                | EarlyCall::GetBlob(_)
                | EarlyCall::GetDiff(_) => {}
            }
        }
        let methods: Vec<_> = every.iter().map(EarlyCall::method).collect();
        assert_eq!(
            methods,
            [
                "SubscribeInventory",
                "SubscribeSession",
                "ListItems",
                "GetBlob",
                "GetDiff"
            ]
        );
        for writes in [
            "SendInput",
            "StopAgent",
            "CreateAgent",
            "RenameAgent",
            "ResumeAgent",
            "DeleteAgent",
            "SendMessage",
            "PutBlob",
            "Pair",
        ] {
            assert!(!methods.contains(&writes), "{writes} cannot be early data");
        }
    }

    mod early {
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;

        use tonic::{Request, Response, Status};
        use wire::peer_agent_service_server::PeerAgentService;
        use wire::peer_session_service_server::PeerSessionService;

        use super::*;
        use crate::identity::DeviceIdentity;
        use crate::link::link_log::EarlyData;
        use crate::link::resumption::{HOST_TICKETS, SessionTickets};
        use crate::link::{LinkEndpoint, QuicCarrier};
        use crate::routing::{Capabilities, Host, LiveLocalHost};
        use crate::transport::BoxedGrpcIo;

        /// Counts the calls a host answered, by method.
        #[derive(Clone, Default)]
        struct Answering {
            inventory: Arc<AtomicUsize>,
            sessions: Arc<AtomicUsize>,
            fetches: Arc<AtomicUsize>,
        }

        type Answer<T> = Result<Response<T>, Status>;
        type Events<T> =
            Pin<Box<dyn futures_util::Stream<Item = Result<T, Status>> + Send + 'static>>;

        fn unanswered<T>() -> Answer<T> {
            Err(Status::unimplemented("not answered here"))
        }

        #[tonic::async_trait]
        impl PeerAgentService for Answering {
            type SubscribeInventoryStream = Events<wire::InventoryEvent>;

            async fn subscribe_inventory(
                &self,
                _: Request<wire::SubscribeInventoryRequest>,
            ) -> Answer<Self::SubscribeInventoryStream> {
                self.inventory.fetch_add(1, Ordering::SeqCst);
                let event = wire::InventoryEvent::default();
                Ok(Response::new(Box::pin(futures_util::stream::iter([Ok(
                    event,
                )]))))
            }

            async fn create_agent(
                &self,
                _: Request<wire::CreateAgentRequest>,
            ) -> Answer<wire::Agent> {
                unanswered()
            }

            async fn rename_agent(
                &self,
                _: Request<wire::RenameAgentRequest>,
            ) -> Answer<wire::Agent> {
                unanswered()
            }

            async fn stop_agent(
                &self,
                _: Request<wire::StopAgentRequest>,
            ) -> Answer<wire::StopAgentResponse> {
                unanswered()
            }

            async fn resume_agent(
                &self,
                _: Request<wire::ResumeAgentRequest>,
            ) -> Answer<wire::Agent> {
                unanswered()
            }

            async fn delete_agent(
                &self,
                _: Request<wire::DeleteAgentRequest>,
            ) -> Answer<wire::Empty> {
                unanswered()
            }

            async fn send_message(
                &self,
                _: Request<wire::PeerSendMessageRequest>,
            ) -> Answer<wire::SendMessageResponse> {
                unanswered()
            }

            async fn put_blob(&self, _: Request<wire::PutBlobRequest>) -> Answer<wire::BlobRef> {
                unanswered()
            }

            async fn get_blob(
                &self,
                _: Request<wire::GetBlobRequest>,
            ) -> Answer<wire::GetBlobResponse> {
                unanswered()
            }

            async fn get_diff(&self, _: Request<wire::GetDiffRequest>) -> Answer<wire::Diff> {
                unanswered()
            }

            async fn get_catalogue(
                &self,
                _: Request<wire::GetCatalogueRequest>,
            ) -> Answer<wire::GetCatalogueResponse> {
                unanswered()
            }

            async fn list_repositories(
                &self,
                _: Request<wire::ListRepositoriesRequest>,
            ) -> Answer<wire::ListRepositoriesResponse> {
                unanswered()
            }
        }

        #[tonic::async_trait]
        impl PeerSessionService for Answering {
            type SubscribeSessionStream = Events<wire::SessionEvent>;

            async fn subscribe_session(
                &self,
                _: Request<wire::PeerSubscribeSessionRequest>,
            ) -> Answer<Self::SubscribeSessionStream> {
                self.sessions.fetch_add(1, Ordering::SeqCst);
                let event = wire::SessionEvent::default();
                Ok(Response::new(Box::pin(futures_util::stream::iter([Ok(
                    event,
                )]))))
            }

            async fn list_items(
                &self,
                _: Request<wire::ListItemsRequest>,
            ) -> Answer<wire::ListItemsResponse> {
                self.fetches.fetch_add(1, Ordering::SeqCst);
                Ok(Response::new(wire::ListItemsResponse::default()))
            }

            async fn get_item(&self, _: Request<wire::GetItemRequest>) -> Answer<wire::Item> {
                unanswered()
            }

            async fn send_input(
                &self,
                _: Request<wire::SendInputRequest>,
            ) -> Answer<wire::SendInputResponse> {
                unanswered()
            }
        }

        fn host_named(identity: &DeviceIdentity) -> Host {
            Host {
                id: identity.host_id,
                name: identity.host_id.to_string(),
                version: "test".to_string(),
                capabilities: Capabilities::default(),
                signed_in: Some(false),
                platform: wire::Platform::Unspecified,
            }
        }

        fn trust_for(peer: &DeviceIdentity) -> SharedTrustStore {
            let mut trust = crate::trust::TrustStore::default();
            trust
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "peer".to_string(),
                    None,
                    chrono::Utc::now(),
                )
                .unwrap();
            Arc::new(std::sync::RwLock::new(trust))
        }

        /// A host answering links on loopback with the peer service, issuing
        /// tickets from `tickets`. What it answered, and for each stream a
        /// dialler opened on a link, whether it arrived as early data.
        struct LoopbackHost {
            endpoint: LinkEndpoint,
            answering: Answering,
            arrived_early: Arc<std::sync::Mutex<Vec<bool>>>,
        }

        impl LoopbackHost {
            fn start(
                identity: &DeviceIdentity,
                dialler: &DeviceIdentity,
                tickets: &Arc<SessionTickets>,
            ) -> Self {
                let config = identity
                    .quic_server_config(trust_for(dialler), tickets)
                    .unwrap();
                let endpoint =
                    LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), Some(config)).unwrap();
                let answering = Answering::default();
                let arrived_early = Arc::<std::sync::Mutex<Vec<bool>>>::default();
                let (inbound_tx, mut inbound) = mpsc::channel::<super::super::InboundStream>(16);
                let (served_tx, mut served) = mpsc::channel::<BoxedGrpcIo>(16);
                let incoming = futures_util::stream::poll_fn(move |cx| {
                    served.poll_recv(cx).map(|io| io.map(Ok::<_, io::Error>))
                });
                tokio::spawn(
                    tonic::transport::Server::builder()
                        .add_service(wire::peer_agent_service_server(answering.clone()))
                        .add_service(wire::peer_session_service_server(answering.clone()))
                        .serve_with_incoming(incoming),
                );
                let early = arrived_early.clone();
                tokio::spawn(async move {
                    while let Some(stream) = inbound.recv().await {
                        early.lock().unwrap().push(stream.stream.arrived_early());
                        let io = BoxedGrpcIo::trusted(
                            crate::transport::Compressed::new(stream.stream),
                            stream.adjacent_peer,
                        );
                        if served_tx.send(io).await.is_err() {
                            return;
                        }
                    }
                });
                let accepting = endpoint.endpoint.clone();
                let (local, peer) = (host_named(identity), dialler.host_id);
                tokio::spawn(async move {
                    while let Some(incoming) = accepting.accept().await {
                        // Taken before the handshake finishes, so a stream
                        // that arrived as early data says so.
                        let Ok(connecting) = incoming.accept() else {
                            continue;
                        };
                        let Ok((connection, _)) = connecting.into_0rtt() else {
                            continue;
                        };
                        let links = Arc::new(LinkRegistry::new(local.id));
                        let ctx = crate::link::LinkCtx::new_live(
                            LiveLocalHost::new(local.clone()),
                            links,
                        )
                        .with_authenticated_peer(peer)
                        .with_incoming_streams(inbound_tx.clone());
                        tokio::spawn(crate::link::run_link(
                            ctx,
                            Arc::new(QuicCarrier::from_accepted(connection)),
                            crate::routing::ConnectRole::Acceptor,
                        ));
                    }
                });
                Self {
                    endpoint,
                    answering,
                    arrived_early,
                }
            }

            fn arrived_early(&self) -> Vec<bool> {
                std::mem::take(&mut *self.arrived_early.lock().unwrap())
            }
        }

        /// Dials one host at an address, resuming where the host gave this
        /// endpoint a ticket.
        struct LoopbackDial {
            endpoint: LinkEndpoint,
            identity: DeviceIdentity,
            trust: SharedTrustStore,
            local: Host,
            links: Arc<LinkRegistry>,
            addr: std::sync::Mutex<std::net::SocketAddr>,
        }

        impl ResumedDial for LoopbackDial {
            fn dial_resumed(
                &self,
                peer: HostId,
                _: Flight,
            ) -> BoxFuture<'_, Result<Resuming, ChannelError>> {
                Box::pin(async move {
                    let addr = *self.addr.lock().unwrap();
                    let carrier = Arc::new(
                        QuicCarrier::resume_direct(
                            &self.endpoint,
                            addr,
                            &self.identity,
                            self.trust.clone(),
                            peer,
                        )
                        .await
                        .map_err(|error| ChannelError::Handshake(error.to_string()))?,
                    );
                    let ctx = crate::link::LinkCtx::new_live(
                        LiveLocalHost::new(self.local.clone()),
                        self.links.clone(),
                    )
                    .with_expected_peer(peer);
                    let (_task, established) =
                        crate::routing::spawn_connector(ctx, carrier.clone());
                    let links = self.links.clone();
                    let up = Box::pin(async move {
                        established
                            .await
                            .map_err(|_| ChannelError::Handshake("no link".to_owned()))?
                            .map_err(|status| ChannelError::Handshake(status.to_string()))?;
                        links
                            .native_carrier_to_peer(peer)
                            .await
                            .map(|(link, _)| link)
                            .ok_or(ChannelError::LinkUnavailable { host_id: peer })
                    });
                    Ok(Resuming {
                        carrier,
                        established: up,
                    })
                })
            }
        }

        struct Dialler {
            pool: ChannelPool,
            dial: Arc<LoopbackDial>,
        }

        fn dialler(identity: DeviceIdentity, host: &DeviceIdentity, at: &LoopbackHost) -> Dialler {
            let links = Arc::new(LinkRegistry::new(identity.host_id));
            let trust = trust_for(host);
            let pool = ChannelPool::new(links.clone(), trust.clone());
            let dial = Arc::new(LoopbackDial {
                // Handshakes take a fifth of a second, so the early calls
                // are written before they finish.
                endpoint: crate::link::quic::slow_arrivals_endpoint(Duration::from_millis(200)),
                local: host_named(&identity),
                identity,
                trust,
                links,
                addr: std::sync::Mutex::new(at.endpoint.local_addr().unwrap()),
            });
            pool.dial_resumed_with(dial.clone());
            Dialler { pool, dial }
        }

        fn calls() -> Vec<EarlyCall> {
            vec![
                EarlyCall::SubscribeInventory,
                EarlyCall::SubscribeSession {
                    session: b"agent".to_vec(),
                    from: Cursor::Tail(10),
                },
                EarlyCall::ListItems(wire::ListItemsRequest::default()),
            ]
        }

        /// Opens a resumed link to `host` with the early calls, reads every
        /// answer, and closes the link. What became of the early data.
        async fn open_and_read(dialler: &Dialler, host: HostId) -> EarlyData {
            let opened = tokio::time::timeout(
                Duration::from_secs(10),
                dialler
                    .pool
                    .open_resumed_on(host, calls(), Flight::default()),
            )
            .await
            .expect("the link comes up in time")
            .expect("the link comes up");
            let mut answered = Vec::new();
            for reply in opened.replies {
                match reply.await.expect("each early call is answered") {
                    EarlyAnswer::Inventory(mut events) => {
                        events.message().await.unwrap().expect("the inventory");
                        answered.push("inventory");
                    }
                    EarlyAnswer::Session(mut events) => {
                        events.message().await.unwrap().expect("the session");
                        answered.push("session");
                    }
                    EarlyAnswer::Fetched(_) => answered.push("fetch"),
                    other => panic!("an answer to no call made: {other:?}"),
                }
            }
            assert_eq!(answered, ["inventory", "session", "fetch"]);
            let key = ChannelKey {
                peer: host,
                route: Route::Direct(opened.link),
                class: ChannelClass::Calls,
            };
            assert!(
                dialler.pool.by_key.read().unwrap().contains_key(&key),
                "the link's calls channel serves the calls that follow"
            );
            let carrier = dialler
                .pool
                .links
                .native_carrier(&opened.link)
                .await
                .unwrap();
            carrier.close(pb::LinkCloseReason::UserShutdown);
            opened.early_data
        }

        fn answered(host: &LoopbackHost) -> [usize; 3] {
            [
                host.answering.inventory.load(Ordering::SeqCst),
                host.answering.sessions.load(Ordering::SeqCst),
                host.answering.fetches.load(Ordering::SeqCst),
            ]
        }

        #[tokio::test]
        async fn early_calls_with_no_ticket_follow_the_handshake() {
            let identity = DeviceIdentity::for_test(HostId::new_v4());
            let me = DeviceIdentity::for_test(HostId::new_v4());
            let host =
                LoopbackHost::start(&identity, &me, &SessionTickets::in_memory(HOST_TICKETS));
            let dialler = dialler(me, &identity, &host);

            assert_eq!(
                open_and_read(&dialler, identity.host_id).await,
                EarlyData::None
            );
            assert_eq!(
                host.arrived_early(),
                [false, false],
                "a calls and a session stream"
            );
            assert_eq!(answered(&host), [1, 1, 1]);
        }

        #[tokio::test]
        async fn early_calls_a_host_accepts_go_out_before_the_handshake_finishes() {
            let identity = DeviceIdentity::for_test(HostId::new_v4());
            let me = DeviceIdentity::for_test(HostId::new_v4());
            let host =
                LoopbackHost::start(&identity, &me, &SessionTickets::in_memory(HOST_TICKETS));
            let dialler = dialler(me, &identity, &host);
            open_and_read(&dialler, identity.host_id).await;
            host.arrived_early();

            assert_eq!(
                open_and_read(&dialler, identity.host_id).await,
                EarlyData::Accepted
            );
            assert_eq!(
                host.arrived_early(),
                [true, true],
                "both channels' streams arrive as early data"
            );
            assert_eq!(answered(&host), [2, 2, 2], "each call answered once a link");
        }

        #[tokio::test]
        async fn early_calls_a_host_refuses_are_made_again_once() {
            let identity = DeviceIdentity::for_test(HostId::new_v4());
            let me = DeviceIdentity::for_test(HostId::new_v4());
            let host =
                LoopbackHost::start(&identity, &me, &SessionTickets::in_memory(HOST_TICKETS));
            let dialler = dialler(me, &identity, &host);
            open_and_read(&dialler, identity.host_id).await;

            // The host started again, holding none of the tickets it gave.
            let restarted = LoopbackHost::start(
                &identity,
                &dialler.dial.identity,
                &SessionTickets::in_memory(HOST_TICKETS),
            );
            *dialler.dial.addr.lock().unwrap() = restarted.endpoint.local_addr().unwrap();
            assert_eq!(
                open_and_read(&dialler, identity.host_id).await,
                EarlyData::Rejected
            );
            assert_eq!(restarted.arrived_early(), [false, false]);
            assert_eq!(
                answered(&restarted),
                [1, 1, 1],
                "the refused calls are made again, each once"
            );
        }
    }
}
