//! Runtime Link establishment from discovery and persisted reachabilities.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::HostId;
use crate::connection::ConnectionManager;
use crate::discovery::{Discovery, DiscoveryEvent, FoundHosts};
use crate::identity::DeviceIdentity;
use crate::link::channels::{ForwardedDial, ResumedDial, Resuming, says_revoked};
use crate::link::framed::{FramedKind, StreamEndpoint};
use crate::link::quic::DEVICE_SERVER_NAME;
use crate::link::socket::Path;
use crate::link::{ChannelError, ChannelPool, LinkCarrier as NativeLinkCarrier, QuicCarrier};
use crate::routing::{Host, LinkCarrier, LinkConnectorCtx, LinkId, LiveLocalHost, Route};
use crate::transport::spawn_ssh_relay;
use crate::trust::{Reachability, SharedTrustStore};

const DIRECT_LINK_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);
/// SSH's own connection setup comes ahead of the QUIC handshake through it.
const SSH_LINK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
// Quinn's first Initial PTO is about one second with its default initial RTT.
// Two seconds lets one lost handshake packet retransmit while still moving
// through several black-holed candidate addresses in a few seconds.
const DIRECT_QUIC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a direct link must have lived for the next dial to follow its
/// close at once, and how long the next dial waits otherwise. A link that
/// closes this soon after coming up was most likely refused by the peer, and
/// a redial straight away is refused the same way — often enough, in a loop,
/// to trip the peer's handshake rate limit.
const SHORT_LINK_REDIAL_PAUSE: Duration = Duration::from_secs(1);
/// How long a failed direct dial to a paired peer waits before the next,
/// doubling each time up to [`REDIAL_MAX`]. A machine that restarts may be
/// advertised again before it answers, a browser that saw its advertisement
/// come and go says nothing more, and a peer paired by address across a
/// network discovery cannot see (a VPN, another subnet) is never advertised
/// at all: only dialling its last working address again finds it.
const REDIAL_FIRST: Duration = Duration::from_secs(1);
const REDIAL_MAX: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct ReachabilityLinkConnector {
    inner: Arc<ReachabilityLinkConnectorInner>,
}

/// Dials the hosts the profile's core says to, at the addresses discovery
/// found for them.
pub(crate) struct Dialer(std::sync::Weak<ReachabilityLinkConnectorInner>);

impl Dialer {
    pub(crate) fn dial(&self, peer: HostId) {
        if let Some(inner) = self.0.upgrade() {
            ReachabilityLinkConnector { inner }.dial_found(peer);
        }
    }
}

struct ReachabilityLinkConnectorInner {
    context: ReachabilityLinkContext,
    retained_tasks: Mutex<Vec<JoinHandle<()>>>,
    dialing: Arc<Mutex<HashSet<HostId>>>,
    queued_found: Arc<Mutex<HashSet<HostId>>>,
    direct_enabled: Arc<AtomicBool>,
    direct_shutdown: Mutex<watch::Sender<bool>>,
}

#[derive(Clone)]
struct ReachabilityLinkContext {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
    local_host: LiveLocalHost,
    channels: Arc<ChannelPool>,
    connections: Arc<ConnectionManager>,
    incoming_streams_tx: tokio::sync::mpsc::Sender<crate::link::InboundStream>,
    runtime: Arc<Mutex<Option<ReachabilityRuntime>>>,
    quic_endpoint: Arc<Mutex<Option<crate::link::LinkEndpoint>>>,
    quic_transport: Arc<Mutex<Option<Arc<quinn::TransportConfig>>>>,
}

#[derive(Clone)]
struct ReachabilityRuntime {
    discovery: Arc<dyn Discovery>,
    found_hosts: Arc<FoundHosts>,
    /// Discovery lists only hosts in this scope.
    scope: String,
}

#[derive(Clone)]
struct ReachabilityLinkAttempt {
    peer: HostId,
    reachability: Reachability,
    ordinal: usize,
    /// Failed direct dials to this found peer just before this one.
    redials: u32,
}

impl ReachabilityLinkConnector {
    pub(crate) fn new(
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
        local_host: LiveLocalHost,
        channels: Arc<ChannelPool>,
        connections: Arc<ConnectionManager>,
        incoming_streams_tx: tokio::sync::mpsc::Sender<crate::link::InboundStream>,
    ) -> Self {
        Self {
            inner: Arc::new(ReachabilityLinkConnectorInner {
                context: ReachabilityLinkContext {
                    identity,
                    trust_store,
                    local_host,
                    channels,
                    connections,
                    incoming_streams_tx,
                    runtime: Arc::new(Mutex::new(None)),
                    quic_endpoint: Arc::new(Mutex::new(None)),
                    quic_transport: Arc::new(Mutex::new(None)),
                },
                retained_tasks: Mutex::new(Vec::new()),
                dialing: Arc::new(Mutex::new(HashSet::new())),
                queued_found: Arc::new(Mutex::new(HashSet::new())),
                direct_enabled: Arc::new(AtomicBool::new(true)),
                direct_shutdown: Mutex::new(watch::channel(false).0),
            }),
        }
    }

    pub fn configure(
        &self,
        discovery: Arc<dyn Discovery>,
        found_hosts: Arc<FoundHosts>,
        quic_endpoint: crate::link::LinkEndpoint,
        scope: String,
    ) {
        let inner = &self.inner;
        let dialer = Arc::new(ForwardedDialer {
            inner: Arc::downgrade(inner),
            dialing: Mutex::default(),
        });
        inner.context.channels.dial_forwarded_with(dialer.clone());
        inner.context.channels.dial_resumed_with(dialer);
        *inner.context.runtime.lock().unwrap() = Some(ReachabilityRuntime {
            discovery,
            found_hosts,
            scope,
        });
        *inner.context.quic_endpoint.lock().unwrap() = Some(quic_endpoint);
    }

    pub(crate) fn spawn_startup_links(&self) -> Vec<JoinHandle<()>> {
        let inner = &self.inner;
        let attempts = match snapshot_attempts(&inner.context) {
            Ok(attempts) => attempts,
            Err(()) => {
                tracing::warn!("failed to read trust store for reachability Link startup");
                return Vec::new();
            }
        };
        attempts
            .into_iter()
            .filter_map(|attempt| self.spawn_attempt(attempt))
            .collect()
    }

    /// Records what discovery finds in this profile's scope and, when `dial`
    /// is set, dials the trusted hosts among them.
    /// Tells the profile's core of every host discovery resolves in this
    /// profile's scope, and from now on dials those it says to while `dial`
    /// holds and direct links are on.
    pub fn spawn_dial_on_found(
        &self,
        mut events: broadcast::Receiver<DiscoveryEvent>,
        dial: bool,
    ) -> JoinHandle<()> {
        let inner = &self.inner;
        let context = inner.context.clone();
        if dial {
            context
                .connections
                .set_dialer(Dialer(Arc::downgrade(inner)));
        }
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let Some(runtime) = context.runtime.lock().unwrap().clone() else {
                            continue;
                        };
                        // A host in another scope is not a candidate; one
                        // that moved scope leaves the list.
                        let event = match event {
                            DiscoveryEvent::Found(advert) if advert.scope != runtime.scope => {
                                DiscoveryEvent::Lost {
                                    host_id: advert.host_id,
                                }
                            }
                            event => event,
                        };
                        let found = match &event {
                            DiscoveryEvent::Found(advert) => Some(advert.clone()),
                            DiscoveryEvent::Lost { .. } => None,
                        };
                        runtime.found_hosts.apply(event);
                        context
                            .connections
                            .candidates(runtime.found_hosts.candidates());
                        let Some(advert) = found else { continue };
                        // Whether to dial is the profile core's to decide.
                        let trusted = is_trusted(&context.trust_store, advert.host_id);
                        context.connections.discovered(advert.host_id, trusted);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(runtime) = context.runtime.lock().unwrap().clone() {
                            runtime.discovery.requery();
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    pub fn spawn_pair_time_link(&self, peer: HostId, reachability: Reachability) {
        if matches!(reachability, Reachability::Cloud) {
            return;
        }
        let Some(task) = self.spawn_attempt(ReachabilityLinkAttempt {
            peer,
            reachability,
            ordinal: 0,
            redials: 0,
        }) else {
            return;
        };
        self.retain_task(task);
    }

    fn dial_found(&self, peer: HostId) {
        let inner = &self.inner;
        if !inner.direct_enabled.load(Ordering::SeqCst) {
            return;
        }
        let addrs = direct_candidates(&inner.context, peer);
        if let Some(task) = self.spawn_attempt(ReachabilityLinkAttempt {
            peer,
            reachability: Reachability::Direct { addrs },
            ordinal: 0,
            redials: 0,
        }) {
            self.retain_task(task);
            return;
        }
        if !inner.queued_found.lock().unwrap().insert(peer) {
            return;
        }

        // A stored-address attempt may already be in flight when discovery
        // resolves the peer. Wait for that attempt to settle, then give the
        // freshly found addresses their own chance instead of losing Found.
        let connector = self.clone();
        let context = inner.context.clone();
        let dialing = inner.dialing.clone();
        let queued_found = inner.queued_found.clone();
        let enabled = inner.direct_enabled.clone();
        let task = tokio::spawn(async move {
            loop {
                if !enabled.load(Ordering::SeqCst) || has_direct_route(&context, peer).await {
                    queued_found.lock().unwrap().remove(&peer);
                    return;
                }
                if !dialing.lock().unwrap().contains(&peer) {
                    queued_found.lock().unwrap().remove(&peer);
                    connector.dial_found(peer);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        self.retain_task(task);
    }

    /// Dials `peer` at the addresses discovery found and the profile
    /// remembers for it, where there are any, redialling while it is
    /// away: what a host the first flight did not reach is left to.
    pub(crate) fn dial_direct(&self, peer: HostId) {
        let inner = &self.inner;
        if !direct_candidates(&inner.context, peer).is_empty() {
            self.dial_found(peer);
        }
    }

    pub(crate) fn requery(&self) {
        let inner = &self.inner;
        if let Some(runtime) = inner.context.runtime.lock().unwrap().clone() {
            runtime.discovery.requery();
        }
    }

    /// Records the whole set an outside browser resolved, so a caller that
    /// hands one over and then asks what it may pair with is answered from
    /// that set rather than from whatever the browse task has caught up with.
    pub(crate) fn hand_over_found(&self, found: Vec<crate::discovery::Advertisement>) {
        let inner = &self.inner;
        if let Some(runtime) = inner.context.runtime.lock().unwrap().as_ref() {
            runtime.found_hosts.hand_over(found);
            inner
                .context
                .connections
                .candidates(runtime.found_hosts.candidates());
        }
    }

    pub fn found_addrs(&self, peer: HostId) -> Vec<std::net::SocketAddr> {
        let inner = &self.inner;
        inner
            .context
            .runtime
            .lock()
            .unwrap()
            .as_ref()
            .map(|runtime| runtime.found_hosts.addrs_for(peer))
            .unwrap_or_default()
    }

    pub(crate) async fn close_direct_links(&self) {
        let inner = &self.inner;
        inner.direct_enabled.store(false, Ordering::SeqCst);
        inner.direct_shutdown.lock().unwrap().send_replace(true);
        inner
            .context
            .channels
            .link_registry()
            .close_peer_links()
            .await;
        let tasks = inner
            .retained_tasks
            .lock()
            .map(|mut tasks| tasks.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        inner.dialing.lock().unwrap().clear();
        inner.queued_found.lock().unwrap().clear();
    }

    fn spawn_attempt(&self, attempt: ReachabilityLinkAttempt) -> Option<JoinHandle<()>> {
        let inner = &self.inner;
        let direct = matches!(attempt.reachability, Reachability::Direct { .. });
        if direct {
            if !inner.direct_enabled.load(Ordering::SeqCst) {
                return None;
            }
            let mut dialing = inner.dialing.lock().unwrap();
            if !dialing.insert(attempt.peer) {
                return None;
            }
        }
        let context = inner.context.clone();
        let dialing = inner.dialing.clone();
        let shutdown_rx = inner.direct_shutdown.lock().unwrap().subscribe();
        let pause_shutdown_rx = shutdown_rx.clone();
        let connector = Arc::downgrade(inner);
        let redials = attempt.redials;
        let span = tracing::info_span!(
            "reachability_link",
            // Which profile is dialling: a phone runs one runtime per account
            // beside the one it draws with nobody signed in, and they share a
            // log.
            local = %inner.context.identity.host_id,
            peer = %attempt.peer,
            reachability = ?attempt.reachability,
            ordinal = attempt.ordinal,
        );
        Some(tokio::spawn(
            async move {
                let peer = attempt.peer;
                tracing::debug!("dialling a reachability of this peer");
                let established_direct =
                    establish_reachability_link(context.clone(), attempt, shutdown_rx).await;
                if direct {
                    dialing.lock().unwrap().remove(&peer);
                    if let Some(established) = established_direct {
                        // Asking the network again finds the peer where it
                        // is advertised, and dialling its addresses again
                        // where it is not; after a link that was refused as
                        // it came up, either would be refused the same way.
                        pause_after_short_link(established, pause_shutdown_rx.clone()).await;
                        if let Some(runtime) = context.runtime.lock().unwrap().clone() {
                            runtime.discovery.requery();
                        }
                        redial_while_away(connector, context, peer, 0, pause_shutdown_rx).await;
                    } else {
                        redial_while_away(connector, context, peer, redials, pause_shutdown_rx)
                            .await;
                    }
                }
            }
            .instrument(span),
        ))
    }

    fn retain_task(&self, task: JoinHandle<()>) {
        let inner = &self.inner;
        let Ok(mut tasks) = inner.retained_tasks.lock() else {
            task.abort();
            tracing::warn!("failed to retain reachability Link task");
            return;
        };
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
}

impl Drop for ReachabilityLinkConnectorInner {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.retained_tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

fn snapshot_attempts(
    context: &ReachabilityLinkContext,
) -> Result<Vec<ReachabilityLinkAttempt>, ()> {
    let store = context.trust_store.read().map_err(|_| ())?;
    let mut attempts = Vec::new();
    for (peer, entry) in store.entries() {
        // A launch's first flight takes the relay and every address at
        // once; only SSH is dialled here.
        for (ordinal, reachability) in entry
            .reachabilities
            .iter()
            .filter(|reachability| matches!(reachability, Reachability::Ssh { .. }))
            .cloned()
            .enumerate()
        {
            attempts.push(ReachabilityLinkAttempt {
                peer,
                reachability,
                ordinal,
                redials: 0,
            });
        }
    }
    Ok(attempts)
}

fn direct_candidates(context: &ReachabilityLinkContext, peer: HostId) -> Vec<std::net::SocketAddr> {
    let mut addrs = context
        .runtime
        .lock()
        .unwrap()
        .as_ref()
        .map(|runtime| runtime.found_hosts.addrs_for(peer))
        .unwrap_or_default();
    if let Ok(store) = context.trust_store.read()
        && let Some(entry) = store
            .entries()
            .find_map(|(id, entry)| (id == peer).then_some(entry))
    {
        for addr in entry
            .reachabilities
            .iter()
            .flat_map(|reachability| match reachability {
                Reachability::Direct { addrs } => addrs.as_slice(),
                _ => &[],
            })
        {
            if !addrs.contains(addr) {
                addrs.push(*addr);
            }
        }
    }
    addrs
}

fn is_trusted(trust_store: &SharedTrustStore, peer: HostId) -> bool {
    trust_store
        .read()
        .map(|store| store.pubkey_for_host(peer).is_some())
        .unwrap_or(false)
}

/// Dials links through forwarders for the channel pool: one dial to a
/// host at a time, the others waiting on it.
struct ForwardedDialer {
    inner: std::sync::Weak<ReachabilityLinkConnectorInner>,
    dialing: Mutex<std::collections::HashMap<HostId, Arc<tokio::sync::Mutex<()>>>>,
}

impl ForwardedDialer {
    fn endpoint(
        &self,
    ) -> Result<
        (
            Arc<ReachabilityLinkConnectorInner>,
            crate::link::LinkEndpoint,
        ),
        ChannelError,
    > {
        let unavailable = || ChannelError::Handshake("the edge is not running".to_owned());
        let inner = self.inner.upgrade().ok_or_else(unavailable)?;
        let endpoint = inner
            .context
            .quic_endpoint
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(unavailable)?;
        Ok((inner, endpoint))
    }

    async fn dial(&self, peer: HostId, through: LinkId) -> Result<(), ChannelError> {
        let (inner, endpoint) = self.endpoint()?;
        let context = &inner.context;
        let one_at_a_time = self
            .dialing
            .lock()
            .unwrap()
            .entry(peer)
            .or_default()
            .clone();
        let _dialing = one_at_a_time.lock().await;
        let registry = context.channels.link_registry();
        if registry.native_carrier_to_peer(peer).await.is_some() {
            return Ok(());
        }
        let refusals = registry.forward_refusals();
        let dialled = refused_or(
            refusals,
            registry.as_ref(),
            peer,
            QuicCarrier::connect_through(
                &endpoint,
                through.core(),
                &context.identity,
                context.trust_store.clone(),
                peer,
            ),
        )
        .await?
        .map_err(|error| dial_failed(context, peer, error))?;
        let shutdown = inner.direct_shutdown.lock().unwrap().subscribe();
        let (_, connector_task, abort_on_drop) = establish_carrier(
            context,
            peer,
            Arc::new(dialled),
            LinkCarrier::Forwarded,
            shutdown,
        )
        .await?;
        tokio::spawn(await_connector(connector_task, abort_on_drop));
        Ok(())
    }

    async fn pair(
        &self,
        peer: HostId,
        through: LinkId,
    ) -> Result<tonic::transport::Channel, ChannelError> {
        let (inner, endpoint) = self.endpoint()?;
        let registry = inner.context.channels.link_registry();
        let pairing = refused_or(
            registry.forward_refusals(),
            registry.as_ref(),
            peer,
            crate::transport::pairing_quic_channel_through(&endpoint, peer, through.core()),
        )
        .await?
        .map_err(|error| ChannelError::Handshake(error.to_string()))?;
        Ok(pairing.channel)
    }
}

/// Runs `dial` under the handshake's deadline, unless the forwarder
/// refuses to carry packets to `peer` first. A dial that times out after a
/// refusal the forwarder has not repeated yet fails with that refusal.
async fn refused_or<T>(
    mut refusals: broadcast::Receiver<(HostId, wire::pb::ForwardRefusal)>,
    registry: &crate::routing::LinkRegistry,
    peer: HostId,
    dial: impl std::future::Future<Output = T>,
) -> Result<T, ChannelError> {
    let refused = async {
        loop {
            match refusals.recv().await {
                Ok((host, reason)) if host == peer => return reason,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    std::future::pending::<()>().await;
                }
            }
        }
    };
    tokio::select! {
        reason = refused => Err(ChannelError::Refused(reason)),
        dialled = tokio::time::timeout(DIRECT_QUIC_HANDSHAKE_TIMEOUT, dial) => {
            dialled.map_err(|_| match registry.last_refusal(peer) {
                Some(reason) => ChannelError::Refused(reason),
                None => ChannelError::Handshake("the QUIC handshake timed out".to_owned()),
            })
        }
    }
}

impl ForwardedDialer {
    /// Dials `peer` resuming with its ticket on every path it may be
    /// reached by at once: the addresses discovery found and the profile
    /// remembers for it (the last that worked among them), the forwarders
    /// a route to it names and, for a host on the account, the relay, even
    /// before the relay has said the host is there. The first path the
    /// host answers on carries the link. Other dials to the host wait for
    /// this one.
    async fn resume(&self, peer: HostId) -> Result<Resuming, ChannelError> {
        let (inner, endpoint) = self.endpoint()?;
        let context = &inner.context;
        let registry = context.channels.link_registry();
        let one_at_a_time = self
            .dialing
            .lock()
            .unwrap()
            .entry(peer)
            .or_default()
            .clone();
        let dialing = one_at_a_time.lock_owned().await;
        // A direct dial discovery asks for meanwhile waits for this one.
        let direct = DirectDial::claim(&inner.dialing, peer);
        if registry.native_carrier_to_peer(peer).await.is_some() {
            return Err(ChannelError::Handshake(
                "a link to the host is already up".to_owned(),
            ));
        }
        let mut paths: Vec<Path> = direct_candidates(context, peer)
            .into_iter()
            .map(Path::Direct)
            .collect();
        for route in context.connections.routes_to(peer).await {
            if let Route::Via(forwarder) = route
                && let Some((link, _)) = registry.native_carrier_to_peer(forwarder).await
            {
                paths.push(Path::Forwarded(link.core()));
            }
        }
        let through_relay = on_the_account(&context.trust_store, peer);
        if paths.is_empty() && !through_relay {
            return Err(ChannelError::NoRoute { host_id: peer });
        }
        tracing::debug!(%peer, ?paths, through_relay, "sending a first flight");
        let carrier = Arc::new(
            QuicCarrier::first_flight(
                &endpoint,
                paths,
                through_relay,
                &context.identity,
                context.trust_store.clone(),
                peer,
            )
            .await
            .map_err(|error| dial_failed(context, peer, error))?,
        );
        // The carrier names the path its link came up on: the connector
        // reads it there.
        let connector_ctx =
            LinkConnectorCtx::new_live(context.local_host.clone(), registry.clone())
                .with_expected_peer(peer)
                .with_carrier(LinkCarrier::Direct)
                .with_incoming_streams(context.incoming_streams_tx.clone());
        let shutdown = inner.direct_shutdown.lock().unwrap().subscribe();
        let pause_shutdown = shutdown.clone();
        let (connector_task, established) =
            crate::routing::spawn_connector_with_establishment_and_shutdown(
                connector_ctx,
                carrier.clone(),
                shutdown,
            );
        let abort_on_failure = AbortTaskOnDrop(connector_task.abort_handle());
        let connector = self.inner.clone();
        let context = context.clone();
        let handshake = carrier.clone();
        let link_carrier = carrier.clone();
        let established = Box::pin(async move {
            let _dialing = (dialing, direct);
            if let Err(error) = handshake.handshake().await {
                return Err(ChannelError::from(dial_failed(&context, peer, error)));
            }
            match tokio::time::timeout(DIRECT_LINK_ESTABLISHMENT_TIMEOUT, established).await {
                Ok(Ok(Ok(_))) => {}
                // The host refused the identity the ticket named.
                Ok(Ok(Err(status))) if says_revoked(&status) => {
                    return Err(ChannelError::from(revoked(&context, peer)));
                }
                Ok(Ok(Err(status))) => return Err(ChannelError::Handshake(status.to_string())),
                Ok(Err(_)) | Err(_) => {
                    return Err(ChannelError::Handshake(
                        "the resumed link did not come up".to_owned(),
                    ));
                }
            }
            // The flight's own link, not any link to the peer: one the peer
            // dialled meanwhile may have won the crossing, and this link
            // was then refused or displaced. What the flight sent early
            // rides this link alone, so it is handed to no call then.
            let flown: Arc<dyn NativeLinkCarrier> = link_carrier.clone();
            let link = registry.link_carried_by(&flown).await.ok_or_else(|| {
                ChannelError::Handshake("a link to the host is already up".to_owned())
            })?;
            if let Some(addr) = link_carrier
                .remote_address()
                .and_then(|stand_in| endpoint.socket.path_of(stand_in))
                .and_then(|path| match path {
                    Path::Direct(addr) => Some(addr),
                    _ => None,
                })
            {
                persist_working_addr(&context, peer, addr);
            }
            context.connections.clear_reachability_error(peer).await;
            tokio::spawn(redial_after(
                connector,
                context,
                peer,
                connector_task,
                abort_on_failure,
                pause_shutdown,
            ));
            Ok(link)
        });
        Ok(Resuming {
            carrier,
            established,
        })
    }
}

/// A host marked as being dialled directly until this is dropped, unless
/// a dial already had it.
struct DirectDial(Option<(Arc<Mutex<HashSet<HostId>>>, HostId)>);

impl DirectDial {
    fn claim(dialing: &Arc<Mutex<HashSet<HostId>>>, peer: HostId) -> Self {
        Self(
            dialing
                .lock()
                .unwrap()
                .insert(peer)
                .then(|| (dialing.clone(), peer)),
        )
    }
}

impl Drop for DirectDial {
    fn drop(&mut self) {
        if let Some((dialing, peer)) = self.0.take() {
            dialing.lock().unwrap().remove(&peer);
        }
    }
}

/// Waits for a link to `peer` to close, then dials it again as one that
/// came up at its address would be.
async fn redial_after(
    connector: std::sync::Weak<ReachabilityLinkConnectorInner>,
    context: ReachabilityLinkContext,
    peer: HostId,
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    abort_on_drop: AbortTaskOnDrop,
    shutdown_rx: watch::Receiver<bool>,
) {
    let established = tokio::time::Instant::now();
    await_connector(connector_task, abort_on_drop).await;
    if *shutdown_rx.borrow() {
        return;
    }
    pause_after_short_link(established, shutdown_rx.clone()).await;
    if let Some(runtime) = context.runtime.lock().unwrap().clone() {
        runtime.discovery.requery();
    }
    redial_while_away(connector, context, peer, 0, shutdown_rx).await;
}

/// Whether `peer` is a host on the account, which the relay reaches.
fn on_the_account(trust_store: &SharedTrustStore, peer: HostId) -> bool {
    trust_store.read().is_ok_and(|store| {
        store.entries().any(|(id, entry)| {
            id == peer
                && entry
                    .reachabilities
                    .iter()
                    .any(|reachability| matches!(reachability, Reachability::Cloud))
        })
    })
}

impl ResumedDial for ForwardedDialer {
    fn dial_resumed(&self, peer: HostId) -> BoxFuture<'_, Result<Resuming, ChannelError>> {
        Box::pin(self.resume(peer))
    }
}

impl ForwardedDial for ForwardedDialer {
    fn dial_through(
        &self,
        peer: HostId,
        through: LinkId,
    ) -> BoxFuture<'_, Result<(), ChannelError>> {
        Box::pin(self.dial(peer, through))
    }

    fn pair_through(
        &self,
        peer: HostId,
        through: LinkId,
    ) -> BoxFuture<'_, Result<tonic::transport::Channel, ChannelError>> {
        Box::pin(self.pair(peer, through))
    }
}

async fn has_direct_route(context: &ReachabilityLinkContext, peer: HostId) -> bool {
    context
        .channels
        .link_registry()
        .has_direct_peer_link_to(peer)
        .await
        || context
            .connections
            .routes_to(peer)
            .await
            .iter()
            .any(|route| matches!(route, Route::Direct(_)))
}

async fn establish_reachability_link(
    context: ReachabilityLinkContext,
    attempt: ReachabilityLinkAttempt,
    shutdown_rx: watch::Receiver<bool>,
) -> Option<tokio::time::Instant> {
    match attempt.reachability.clone() {
        Reachability::Cloud => None,
        Reachability::Direct { addrs } => {
            // Each address's failure, so the person learns why every one
            // of them was refused and not only the last.
            let mut failures = Vec::new();
            for addr in addrs {
                let prepared = tokio::time::timeout(
                    DIRECT_QUIC_HANDSHAKE_TIMEOUT,
                    prepare_direct_carrier(&context, attempt.peer, addr),
                )
                .await
                .map_err(|_| DialError::Failed("the QUIC handshake timed out".to_owned()))
                .and_then(|result| result);
                match prepared {
                    Ok(carrier) => match establish_carrier(
                        &context,
                        attempt.peer,
                        carrier,
                        LinkCarrier::Direct,
                        shutdown_rx.clone(),
                    )
                    .await
                    {
                        Ok((host, connector_task, abort_on_drop)) => {
                            persist_working_addr(&context, attempt.peer, addr);
                            context
                                .connections
                                .clear_reachability_error(attempt.peer)
                                .await;
                            tracing::info!(peer_name = %host.name, %addr, "direct Link established");
                            let established = tokio::time::Instant::now();
                            await_connector(connector_task, abort_on_drop).await;
                            return Some(established);
                        }
                        Err(error) => failures.push(format!("{addr}: {error}")),
                    },
                    Err(error) => failures.push(format!("{addr}: {error}")),
                }
            }
            let error = if failures.is_empty() {
                "no direct addresses available".to_owned()
            } else {
                failures.join("; ")
            };
            context
                .connections
                .record_reachability_error(attempt.peer, error.clone())
                .await;
            tracing::warn!(error = %error, "direct Link establishment failed");
            None
        }
        Reachability::Ssh { target, profile } => {
            let carrier = match tokio::time::timeout(
                SSH_LINK_HANDSHAKE_TIMEOUT,
                prepare_ssh_carrier(&context, attempt.peer, &target, profile),
            )
            .await
            .map_err(|_| DialError::Failed("the QUIC handshake over SSH timed out".to_owned()))
            .and_then(|result| result)
            {
                Ok(carrier) => carrier,
                Err(error) => {
                    context
                        .connections
                        .record_reachability_error(attempt.peer, error.to_string())
                        .await;
                    tracing::warn!(error = %error, "failed to prepare SSH Link transport");
                    return None;
                }
            };
            match establish_carrier(
                &context,
                attempt.peer,
                carrier,
                LinkCarrier::Ssh,
                shutdown_rx,
            )
            .await
            {
                Ok((host, connector_task, abort_on_drop)) => {
                    context
                        .connections
                        .clear_reachability_error(attempt.peer)
                        .await;
                    tracing::info!(peer_name = %host.name, "SSH Link established");
                    await_connector(connector_task, abort_on_drop).await;
                }
                Err(error) => {
                    context
                        .connections
                        .record_reachability_error(attempt.peer, error.to_string())
                        .await;
                    tracing::warn!(error = %error, "SSH Link establishment failed");
                }
            }
            None
        }
    }
}

/// Runs `amux relay` on the far host over SSH and dials its link socket
/// through the session, pinning the peer's key as a direct dial does.
async fn prepare_ssh_carrier(
    context: &ReachabilityLinkContext,
    peer: HostId,
    target: &str,
    profile: crate::ProfileId,
) -> Result<Arc<dyn NativeLinkCarrier>, DialError> {
    let failed = |error: &dyn std::fmt::Display| DialError::Failed(error.to_string());
    let io = spawn_ssh_relay(target, profile).map_err(|error| failed(&error))?;
    let stream = StreamEndpoint::new(io, FramedKind::Ssh, None).map_err(|error| failed(&error))?;
    let config = context
        .identity
        .quic_client_config_for_peer(context.trust_store.clone(), peer)
        .map_err(|error| failed(&error))?;
    QuicCarrier::connect_over_stream(stream, config, DEVICE_SERVER_NAME)
        .await
        .map(|carrier| Arc::new(carrier) as Arc<dyn NativeLinkCarrier>)
        .map_err(|error| dial_failed(context, peer, error))
}

async fn prepare_direct_carrier(
    context: &ReachabilityLinkContext,
    peer: HostId,
    addr: std::net::SocketAddr,
) -> Result<Arc<dyn NativeLinkCarrier>, DialError> {
    let endpoint = context
        .quic_endpoint
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| DialError::Failed("direct QUIC endpoint is not configured".to_owned()))?;
    let transport = context.quic_transport.lock().unwrap().clone();
    QuicCarrier::connect_direct_with_transport(
        &endpoint,
        addr,
        &context.identity,
        context.trust_store.clone(),
        peer,
        transport,
    )
    .await
    .map(|carrier| Arc::new(carrier) as Arc<dyn NativeLinkCarrier>)
    .map_err(|error| dial_failed(context, peer, error))
}

/// Why a link to a peer did not come up.
#[derive(Debug)]
enum DialError {
    /// The peer refused this host's certificate: it no longer trusts this
    /// one. The profile's core has been told.
    Revoked,
    Failed(String),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::Revoked => f.write_str(crate::link::TRUST_REVOKED),
            DialError::Failed(error) => f.write_str(error),
        }
    }
}

impl From<DialError> for ChannelError {
    fn from(error: DialError) -> Self {
        match error {
            DialError::Revoked => ChannelError::Revoked,
            DialError::Failed(error) => ChannelError::Handshake(error),
        }
    }
}

/// The peer's handshake refusing this host's certificate is the peer
/// saying it no longer trusts this one, on whichever route the dial took:
/// a host revoked while its links were down learns it here, when it next
/// dials, as it would from the link's close had it been up.
fn revoked(context: &ReachabilityLinkContext, peer: HostId) -> DialError {
    context.connections.peer_revoked(peer);
    DialError::Revoked
}

fn dial_failed(
    context: &ReachabilityLinkContext,
    peer: HostId,
    error: crate::link::quic::ConnectError,
) -> DialError {
    if error.refused_as_revoked() {
        revoked(context, peer)
    } else {
        DialError::Failed(error.to_string())
    }
}

async fn establish_carrier(
    context: &ReachabilityLinkContext,
    peer: HostId,
    native_carrier: Arc<dyn NativeLinkCarrier>,
    carrier: LinkCarrier,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(Host, JoinHandle<Result<(), tonic::Status>>, AbortTaskOnDrop), DialError> {
    let connector_ctx =
        LinkConnectorCtx::new_live(context.local_host.clone(), context.channels.link_registry())
            .with_expected_peer(peer)
            .with_carrier(carrier)
            .with_incoming_streams(context.incoming_streams_tx.clone());
    let (connector_task, established_rx) =
        crate::routing::spawn_connector_with_establishment_and_shutdown(
            connector_ctx,
            native_carrier.clone(),
            shutdown_rx,
        );
    let abort_on_failure = AbortTaskOnDrop(connector_task.abort_handle());
    let failed = match tokio::time::timeout(DIRECT_LINK_ESTABLISHMENT_TIMEOUT, established_rx).await
    {
        Ok(Ok(Ok(host))) => return Ok((host, connector_task, abort_on_failure)),
        // A dial resuming with a ticket whose key the host no longer pins.
        Ok(Ok(Err(status))) if says_revoked(&status) => return Err(revoked(context, peer)),
        Ok(Ok(Err(status))) => status.to_string(),
        Ok(Err(_)) => "reachability Link task ended before establishment".to_owned(),
        Err(_) => "reachability Link establishment timed out".to_owned(),
    };
    // The handshake seemed done, but the peer refused the certificate
    // after it, as TLS 1.3 lets a server.
    if native_carrier.close_reason() == Some(wire::pb::LinkCloseReason::UserRevoked) {
        return Err(revoked(context, peer));
    }
    Err(DialError::Failed(failed))
}

fn persist_working_addr(
    context: &ReachabilityLinkContext,
    peer: HostId,
    addr: std::net::SocketAddr,
) {
    context.connections.remember_direct_addr(peer, addr);
}

async fn await_connector(
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    _abort_on_drop: AbortTaskOnDrop,
) {
    match connector_task.await {
        Ok(Ok(())) => tracing::info!("reachability Link closed cleanly"),
        Ok(Err(status)) => tracing::warn!(error = %status, "reachability Link closed with error"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => tracing::warn!(error = %error, "reachability Link task panicked"),
    }
}

/// Dials a paired peer again, once the pause for this many failed dials is
/// over, at what discovery lists for it and the address that last worked,
/// unless nothing is known to dial, it is no longer paired, or something
/// else has linked to it directly meanwhile.
async fn redial_while_away(
    connector: std::sync::Weak<ReachabilityLinkConnectorInner>,
    context: ReachabilityLinkContext,
    peer: HostId,
    redials: u32,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let pause = REDIAL_FIRST
        .saturating_mul(1 << redials.min(5))
        .min(REDIAL_MAX);
    tokio::select! {
        () = tokio::time::sleep(pause) => {}
        _ = shutdown_rx.wait_for(|shutting_down| *shutting_down) => return,
    }
    if direct_candidates(&context, peer).is_empty()
        || !is_trusted(&context.trust_store, peer)
        || has_direct_route(&context, peer).await
    {
        return;
    }
    let Some(inner) = connector.upgrade() else {
        return;
    };
    let connector = ReachabilityLinkConnector { inner };
    if let Some(task) = connector.spawn_attempt(ReachabilityLinkAttempt {
        peer,
        reachability: Reachability::Direct {
            addrs: direct_candidates(&context, peer),
        },
        ordinal: 0,
        redials: redials.saturating_add(1),
    }) {
        connector.retain_task(task);
    }
}

/// Holds the next dial back after a link that closed almost as soon as it came
/// up, unless the connector is shutting down.
async fn pause_after_short_link(
    established: tokio::time::Instant,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let resume = established + SHORT_LINK_REDIAL_PAUSE;
    if tokio::time::Instant::now() >= resume {
        return;
    }
    tokio::select! {
        () = tokio::time::sleep_until(resume) => {}
        _ = shutdown_rx.wait_for(|shutting_down| *shutting_down) => {}
    }
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_link_refused_as_it_came_up_holds_the_next_dial_back() {
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let established = tokio::time::Instant::now();
        tokio::time::advance(Duration::from_millis(5)).await;

        pause_after_short_link(established, shutdown_rx).await;

        assert_eq!(established.elapsed(), SHORT_LINK_REDIAL_PAUSE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_link_that_lived_is_redialled_at_once() {
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let established = tokio::time::Instant::now();
        tokio::time::advance(SHORT_LINK_REDIAL_PAUSE * 30).await;
        let closed = tokio::time::Instant::now();

        pause_after_short_link(established, shutdown_rx).await;

        assert_eq!(closed.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn shutting_down_ends_the_pause() {
        let (shutdown, shutdown_rx) = watch::channel(false);
        let established = tokio::time::Instant::now();
        let pause = tokio::spawn(pause_after_short_link(established, shutdown_rx));
        tokio::task::yield_now().await;

        shutdown.send(true).unwrap();
        pause.await.unwrap();

        assert!(established.elapsed() < SHORT_LINK_REDIAL_PAUSE);
    }
}
