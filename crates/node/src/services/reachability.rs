//! Dialling the hosts the profile's core says to.
//!
//! The core decides when a host is dialled, on which paths and for how
//! long, and when it is tried again. This makes each attempt as told: one
//! QUIC connection to the host whose first flight goes out on every path
//! the core gave at once, the first path the host answers on carrying it,
//! and an SSH session beside it where the core gave one. It reports how
//! the attempt ended, and drops it when the core abandons it. It also
//! carries what discovery finds in the profile's scope to the core, and a
//! call's need for a link to a host it has none to.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use daemon_core::payload::PeerRequest;
use daemon_core::{AttemptId, AttemptOutcome, DialPath, Input, Plan};
use futures_util::future::BoxFuture;
use tokio::sync::{OwnedMutexGuard, broadcast, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::HostId;
use crate::connection::ConnectionManager;
use crate::discovery::{Discovery, DiscoveryEvent, FoundHosts};
use crate::edge::first_flight::{FirstFlights, Landed};
use crate::identity::DeviceIdentity;
use crate::link::channels::{Flight, ForwardedDial, ResumedDial, Resuming, says_revoked};
use crate::link::framed::{FramedKind, StreamEndpoint};
use crate::link::quic::DEVICE_SERVER_NAME;
use crate::link::socket::Path;
use crate::link::{ChannelError, ChannelPool, LinkEndpoint, QuicCarrier};
use crate::routing::{LinkConnectorCtx, LinkId, LiveLocalHost, core_host};
use crate::transport::spawn_ssh_relay;
use crate::trust::SharedTrustStore;

/// How long a link's hello may take once its connection is up.
const LINK_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);
/// SSH's own connection setup comes ahead of the QUIC handshake through it.
const SSH_LINK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a handshake may take before the dial gives it up on its own:
/// the core abandons an attempt at its deadline, and this only bounds an
/// attempt whose abandonment never arrives.
const HANDSHAKE_BOUND: Duration = Duration::from_millis(daemon_core::reach::ATTEMPT_DEADLINE_MS);
/// How long a call waits for a link the core dialled for it, past the
/// attempt's own deadline.
const WANT_PATIENCE: Duration =
    Duration::from_millis(daemon_core::reach::ATTEMPT_DEADLINE_MS + 2_000);

/// Makes the attempts the profile's core asks for.
#[derive(Clone)]
pub struct ReachabilityLinkConnector {
    inner: Arc<Inner>,
}

/// What the core's outputs reach the attempts by.
pub(crate) struct Dialer(Weak<Inner>);

impl Dialer {
    pub(crate) fn dial(
        &self,
        host: HostId,
        attempt: AttemptId,
        plan: Plan,
        early: Vec<PeerRequest>,
    ) {
        if let Some(inner) = self.0.upgrade() {
            ReachabilityLinkConnector { inner }.dial(host, attempt, plan, early);
        }
    }

    pub(crate) fn abandon(&self, host: HostId, attempt: AttemptId) {
        if let Some(inner) = self.0.upgrade() {
            inner.abandon(host, Some(attempt));
        }
    }

    pub(crate) fn rediscover(&self) {
        if let Some(inner) = self.0.upgrade() {
            inner.discovery.requery();
        }
    }
}

struct Inner {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
    local_host: LiveLocalHost,
    channels: Arc<ChannelPool>,
    connections: Arc<ConnectionManager>,
    incoming_streams_tx: tokio::sync::mpsc::Sender<crate::link::InboundStream>,
    endpoint: LinkEndpoint,
    discovery: Arc<dyn Discovery>,
    found_hosts: Arc<FoundHosts>,
    /// Discovery lists only hosts in this scope.
    scope: String,
    first_flights: Arc<FirstFlights>,
    /// Each host's attempt out.
    attempts: Mutex<HashMap<HostId, Running>>,
    /// The connectors of links the attempts brought up, until each ends.
    links: Mutex<Vec<JoinHandle<()>>>,
    stopped: AtomicBool,
    shutdown: watch::Sender<bool>,
}

struct Running {
    attempt: AttemptId,
    task: tokio::task::AbortHandle,
    /// How the attempt ended, once it has; closed with nothing said when
    /// the attempt was dropped.
    over: watch::Receiver<Option<AttemptOutcome>>,
}

impl Inner {
    /// Drops `host`'s attempt, the one named or whichever is out.
    fn abandon(&self, host: HostId, attempt: Option<AttemptId>) {
        let mut attempts = self.attempts.lock().unwrap();
        if attempts
            .get(&host)
            .is_some_and(|running| attempt.is_none_or(|attempt| running.attempt == attempt))
            && let Some(running) = attempts.remove(&host)
        {
            running.task.abort();
        }
    }

    fn report(&self, input: Input) {
        if let Some(core) = self.channels.link_registry().core() {
            core.report(input);
        }
    }
}

pub(crate) struct ConnectorParts {
    pub(crate) identity: DeviceIdentity,
    pub(crate) trust_store: SharedTrustStore,
    pub(crate) local_host: LiveLocalHost,
    pub(crate) channels: Arc<ChannelPool>,
    pub(crate) connections: Arc<ConnectionManager>,
    pub(crate) incoming_streams_tx: tokio::sync::mpsc::Sender<crate::link::InboundStream>,
    pub(crate) endpoint: LinkEndpoint,
    pub(crate) discovery: Arc<dyn Discovery>,
    pub(crate) found_hosts: Arc<FoundHosts>,
    pub(crate) scope: String,
    pub(crate) first_flights: Arc<FirstFlights>,
}

impl ReachabilityLinkConnector {
    pub(crate) fn new(parts: ConnectorParts) -> Self {
        let inner = Arc::new(Inner {
            identity: parts.identity,
            trust_store: parts.trust_store,
            local_host: parts.local_host,
            channels: parts.channels,
            connections: parts.connections,
            incoming_streams_tx: parts.incoming_streams_tx,
            endpoint: parts.endpoint,
            discovery: parts.discovery,
            found_hosts: parts.found_hosts,
            scope: parts.scope,
            first_flights: parts.first_flights,
            attempts: Mutex::default(),
            links: Mutex::default(),
            stopped: AtomicBool::new(false),
            shutdown: watch::channel(false).0,
        });
        let dialer = Arc::new(ForwardedDialer(Arc::downgrade(&inner)));
        inner.channels.dial_forwarded_with(dialer.clone());
        inner.channels.dial_resumed_with(dialer);
        inner.connections.set_dialer(Dialer(Arc::downgrade(&inner)));
        Self { inner }
    }

    /// Tells the profile's core of every host discovery resolves in this
    /// profile's scope, as each resolves or goes.
    pub fn spawn_dial_on_found(
        &self,
        mut events: broadcast::Receiver<DiscoveryEvent>,
    ) -> JoinHandle<()> {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let event = events.recv().await;
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                match event {
                    Ok(event) => {
                        // A host in another scope is not a candidate; one
                        // that moved scope leaves the list.
                        let event = match event {
                            DiscoveryEvent::Found(advert) if advert.scope != inner.scope => {
                                DiscoveryEvent::Lost {
                                    host_id: advert.host_id,
                                }
                            }
                            event => event,
                        };
                        inner.found_hosts.apply(event);
                        inner.connections.candidates(inner.found_hosts.candidates());
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => inner.discovery.requery(),
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    /// The core may dial from now on: `direct` says whether it may dial
    /// hosts' own addresses, or stays off the local network, and
    /// `listening` whether hosts may dial this one.
    pub(crate) fn start_dialling(&self, direct: bool, listening: bool) {
        self.inner.stopped.store(false, Ordering::SeqCst);
        self.inner.report(Input::Listening(listening));
        self.inner.report(Input::Dialling(if direct {
            daemon_core::reach::Dialling::All
        } else {
            daemon_core::reach::Dialling::Indirect
        }));
    }

    pub(crate) fn requery(&self) {
        self.inner.discovery.requery();
    }

    /// Records the whole set an outside browser resolved, so a caller that
    /// hands one over and then asks what it may pair with is answered from
    /// that set rather than from whatever the browse task has caught up with.
    pub(crate) fn hand_over_found(&self, found: Vec<crate::discovery::Advertisement>) {
        self.inner.found_hosts.hand_over(found);
        self.inner
            .connections
            .candidates(self.inner.found_hosts.candidates());
    }

    /// Dials nothing more, and drops the attempts under way: a stopping
    /// host that redialled the links its goodbye closes would cross the
    /// peers' own redials.
    pub(crate) fn stop_dialling(&self) {
        let inner = &self.inner;
        inner.stopped.store(true, Ordering::SeqCst);
        inner.report(Input::Dialling(daemon_core::reach::Dialling::Off));
        for (_, running) in inner.attempts.lock().unwrap().drain() {
            running.task.abort();
        }
    }

    /// Stops dialling: the core is told, every attempt out is dropped, and
    /// the links of this profile's own close.
    pub(crate) async fn close_direct_links(&self) {
        self.stop_dialling();
        self.inner.shutdown.send_replace(true);
        self.release_links().await;
    }

    /// Closes the links of this profile's own, and waits for the
    /// connectors of those its attempts brought up to end.
    pub(crate) async fn release_links(&self) {
        let inner = &self.inner;
        inner.channels.link_registry().close_peer_links().await;
        let links = std::mem::take(&mut *inner.links.lock().unwrap());
        for link in &links {
            link.abort();
        }
        for link in links {
            let _ = link.await;
        }
    }

    fn dial(&self, host: HostId, attempt: AttemptId, plan: Plan, early: Vec<PeerRequest>) {
        let inner = &self.inner;
        if inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        let (over_tx, over) = watch::channel(None);
        let span = tracing::info_span!(
            "attempt",
            // Which profile is dialling: a phone runs one runtime per account
            // beside the one it draws with nobody signed in, and they share a
            // log.
            local = %inner.identity.host_id,
            peer = %host,
            attempt = attempt.0,
        );
        // The flight is marked out as the core's dial is handed over, not
        // in the attempt's task: the core's calls of the host follow in
        // later steps, each in a task of its own, and one run before the
        // attempt's would miss the flight and ask the host again.
        let on_socket = plan.through_relay
            || plan
                .paths
                .iter()
                .any(|path| !matches!(path, DialPath::Ssh(_)));
        let landed = on_socket.then(|| inner.first_flights.begin(host));
        let weak = Arc::downgrade(inner);
        let task = tokio::spawn(
            async move {
                tracing::debug!(?plan, "dialling");
                let outcome = attempt_on(&weak, host, plan, early, landed).await;
                tracing::debug!(?outcome, "the attempt ended");
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                {
                    let mut attempts = inner.attempts.lock().unwrap();
                    if attempts
                        .get(&host)
                        .is_some_and(|running| running.attempt == attempt)
                    {
                        attempts.remove(&host);
                    }
                }
                inner.report(Input::AttemptEnded {
                    host: core_host(host),
                    attempt,
                    outcome: outcome.clone(),
                });
                let _ = over_tx.send(Some(outcome));
            }
            .instrument(span),
        );
        let previous = inner.attempts.lock().unwrap().insert(
            host,
            Running {
                attempt,
                task: task.abort_handle(),
                over,
            },
        );
        if let Some(previous) = previous {
            previous.task.abort();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for (_, running) in self.attempts.get_mut().unwrap().drain() {
            running.task.abort();
        }
        for link in self.links.get_mut().unwrap().drain(..) {
            link.abort();
        }
    }
}

/// Makes one attempt on every path of `plan`: the paths on the profile's
/// socket in one connection, and SSH beside it. The first link up is the
/// attempt's; the others are dropped.
async fn attempt_on(
    inner: &Weak<Inner>,
    host: HostId,
    plan: Plan,
    early: Vec<PeerRequest>,
    landed: Option<OwnedMutexGuard<Landed>>,
) -> AttemptOutcome {
    let mut paths = Vec::new();
    let mut ssh = None;
    let planned = plan.paths.clone();
    for path in plan.paths {
        match path {
            DialPath::Direct(addr) => paths.push(Path::Direct(addr)),
            DialPath::Through(link) => paths.push(Path::Forwarded(link)),
            DialPath::Ssh(target) => ssh = Some(target),
        }
    }
    let flight = (!paths.is_empty() || plan.through_relay).then_some(Flight {
        paths,
        through_relay: plan.through_relay,
    });
    let on_socket = async {
        match flight.zip(landed) {
            Some((flight, landed)) => Some(fly(inner, host, flight, early, landed).await),
            None => None,
        }
    };
    let over_ssh = async {
        match ssh {
            Some(target) => Some(dial_ssh(inner, host, &target).await),
            None => None,
        }
    };
    tokio::pin!(on_socket, over_ssh);
    let (first, other) = tokio::select! {
        first = &mut on_socket => (first, Either::Ssh),
        first = &mut over_ssh => (first, Either::Socket),
    };
    let first = match first {
        Some(Ok(outcome)) => return outcome,
        first => first,
    };
    let second = match other {
        Either::Ssh => over_ssh.await,
        Either::Socket => on_socket.await,
    };
    if let Some(Ok(outcome)) = second {
        return outcome;
    }
    let failures: Vec<DialFailure> = [first, second]
        .into_iter()
        .flatten()
        .filter_map(Result::err)
        .collect();
    if failures
        .iter()
        .any(|failure| matches!(failure, DialFailure::Revoked))
    {
        return AttemptOutcome::Revoked;
    }
    // A path of the plan that turned the dial away is the core's to leave
    // out; one it did not name (a link to the relay) is a failure like any.
    let turned = failures.iter().find_map(|failure| match failure {
        DialFailure::TurnedAway { path, error } => {
            let path = match *path {
                Path::Direct(addr) => DialPath::Direct(addr),
                Path::Forwarded(link) => DialPath::Through(link),
                _ => return None,
            };
            planned.contains(&path).then(|| (path, error.clone()))
        }
        _ => None,
    });
    if let Some((path, error)) = turned {
        return AttemptOutcome::TurnedAway { path, error };
    }
    let said: Vec<String> = failures.iter().map(ToString::to_string).collect();
    AttemptOutcome::Failed(if said.is_empty() {
        "nothing to dial".to_owned()
    } else {
        said.join("; ")
    })
}

enum Either {
    Socket,
    Ssh,
}

/// Why an attempt's dial did not bring a link up.
#[derive(Debug)]
enum DialFailure {
    /// The host refused this one's certificate: it no longer trusts it.
    Revoked,
    /// What answered on `path` ended the dial before the other paths could.
    TurnedAway {
        path: Path,
        error: String,
    },
    Failed(String),
}

impl std::fmt::Display for DialFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialFailure::Revoked => f.write_str(crate::link::TRUST_REVOKED),
            DialFailure::TurnedAway { error, .. } | DialFailure::Failed(error) => {
                f.write_str(error)
            }
        }
    }
}

impl From<ChannelError> for DialFailure {
    fn from(error: ChannelError) -> Self {
        match error {
            ChannelError::Revoked => DialFailure::Revoked,
            ChannelError::TurnedAway { path, error } => DialFailure::TurnedAway { path, error },
            error => DialFailure::Failed(error.to_string()),
        }
    }
}

/// The early call a request the core makes of a host is, where it may be
/// one.
fn early_call(request: &PeerRequest) -> Option<crate::link::EarlyCall> {
    use crate::link::EarlyCall;
    match request {
        PeerRequest::SubscribeInventory => Some(EarlyCall::SubscribeInventory),
        PeerRequest::SubscribeSession(request) => Some(EarlyCall::SubscribeSession {
            session: request.agent_id.clone(),
            from: request.from?,
        }),
        _ => None,
    }
}

/// Sends `host` its first flight on every path of `flight` at once,
/// resuming with the ticket its last link left and carrying the calls the
/// core will make of it as early data; their answers wait for those calls.
async fn fly(
    inner: &Weak<Inner>,
    host: HostId,
    flight: Flight,
    early: Vec<PeerRequest>,
    landed: OwnedMutexGuard<Landed>,
) -> Result<AttemptOutcome, DialFailure> {
    let Some(me) = inner.upgrade() else {
        return Err(DialFailure::Failed("the edge is stopping".to_owned()));
    };
    let (requests, calls): (Vec<PeerRequest>, Vec<crate::link::EarlyCall>) = early
        .into_iter()
        .filter_map(|request| Some((early_call(&request)?, request)))
        .map(|(call, request)| (request, call))
        .unzip();
    let channels = me.channels.clone();
    let first_flights = me.first_flights.clone();
    let endpoint = me.endpoint.clone();
    let own = me.identity.host_id;
    drop(me);
    let sent = tokio::time::Instant::now();
    let tried = flight.to_string();
    let resumed = channels
        .open_resumed_on(host, calls, flight)
        .await
        .map_err(|error| match DialFailure::from(error) {
            DialFailure::Failed(error) => DialFailure::Failed(format!("{tried}: {error}")),
            revoked => revoked,
        })?;
    let addr = resumed
        .carrier
        .remote_address()
        .and_then(|stand_in| endpoint.socket.path_of(stand_in))
        .and_then(|path| match path {
            Path::Direct(addr) => Some(addr),
            _ => None,
        });
    first_flights.landed(landed, own, host, sent, requests, resumed);
    Ok(AttemptOutcome::Linked { addr })
}

/// Runs `amux relay` on the far host over SSH, joining the profile whose
/// id is the peer's host id, dials its link socket through the session,
/// pinning the peer's key as a direct dial does, and brings the link up.
async fn dial_ssh(
    inner: &Weak<Inner>,
    peer: HostId,
    target: &str,
) -> Result<AttemptOutcome, DialFailure> {
    let Some(me) = inner.upgrade() else {
        return Err(DialFailure::Failed("the edge is stopping".to_owned()));
    };
    let carrier = tokio::time::timeout(SSH_LINK_HANDSHAKE_TIMEOUT, ssh_carrier(&me, peer, target))
        .await
        .map_err(|_| DialFailure::Failed("the QUIC handshake over SSH timed out".to_owned()))??;
    establish(&me, peer, carrier).await?;
    Ok(AttemptOutcome::Linked { addr: None })
}

async fn ssh_carrier(
    me: &Inner,
    peer: HostId,
    target: &str,
) -> Result<Arc<QuicCarrier>, DialFailure> {
    let failed = |error: &dyn std::fmt::Display| DialFailure::Failed(error.to_string());
    let mut io = spawn_ssh_relay(target, peer).map_err(|error| failed(&error))?;
    let stderr = io.take_stderr();
    let stream = StreamEndpoint::new(io, FramedKind::Ssh, None).map_err(|error| failed(&error))?;
    let config = me
        .identity
        .quic_client_config_for_peer(me.trust_store.clone(), peer)
        .map_err(|error| failed(&error))?;
    match QuicCarrier::connect_over_stream(stream, config, DEVICE_SERVER_NAME).await {
        Ok(carrier) => Ok(Arc::new(carrier)),
        Err(error) if error.refused_as_revoked() => Err(DialFailure::Revoked),
        // A session that ended before the handshake did, and what ssh or
        // the far end said about it, such as amux not being installed.
        Err(error) => Err(DialFailure::Failed(
            crate::transport::with_said(error.to_string(), stderr).await,
        )),
    }
}

/// Brings a link up over `carrier`: its connector says Hello and the host
/// answers. The link lives on in its connector, kept until it ends.
async fn establish(me: &Inner, peer: HostId, carrier: Arc<QuicCarrier>) -> Result<(), DialFailure> {
    let connector_ctx =
        LinkConnectorCtx::new_live(me.local_host.clone(), me.channels.link_registry())
            .with_expected_peer(peer)
            .with_incoming_streams(me.incoming_streams_tx.clone());
    let (connector_task, established) =
        crate::routing::spawn_connector_with_establishment_and_shutdown(
            connector_ctx,
            carrier,
            me.shutdown.subscribe(),
        );
    let abort_on_failure = AbortTaskOnDrop(connector_task.abort_handle());
    match tokio::time::timeout(LINK_ESTABLISHMENT_TIMEOUT, established).await {
        Ok(Ok(Ok(host))) => {
            tracing::info!(peer_name = %host.name, "link established");
            keep_link(me, connector_task, abort_on_failure);
            Ok(())
        }
        // The host refused this one's certificate after the handshake.
        Ok(Ok(Err(status))) if says_revoked(&status) => Err(DialFailure::Revoked),
        Ok(Ok(Err(status))) => Err(DialFailure::Failed(status.to_string())),
        Ok(Err(_)) => Err(DialFailure::Failed(
            "the link's task ended before it came up".to_owned(),
        )),
        Err(_) => Err(DialFailure::Failed(
            "the link's hello was not answered in time".to_owned(),
        )),
    }
}

/// Keeps a link's connector until it ends, so stopping the edge ends it.
fn keep_link(
    me: &Inner,
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    abort_on_drop: AbortTaskOnDrop,
) {
    let mut links = me.links.lock().unwrap();
    links.retain(|link| !link.is_finished());
    links.push(tokio::spawn(await_connector(connector_task, abort_on_drop)));
}

async fn await_connector(
    connector_task: JoinHandle<Result<(), tonic::Status>>,
    _abort_on_drop: AbortTaskOnDrop,
) {
    match connector_task.await {
        Ok(Ok(())) => tracing::info!("link closed cleanly"),
        Ok(Err(status)) => tracing::warn!(error = %status, "link closed with error"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => tracing::warn!(error = %error, "link task panicked"),
    }
}

/// Dials links for the channel pool: a host's first flight, a call's need
/// for a link, and a pairing through a forwarder.
struct ForwardedDialer(Weak<Inner>);

impl ForwardedDialer {
    fn inner(&self) -> Result<Arc<Inner>, ChannelError> {
        self.0
            .upgrade()
            .ok_or_else(|| ChannelError::Handshake("the edge is not running".to_owned()))
    }

    /// Dials `peer` resuming with its ticket on every path of `flight` at
    /// once. The first path the host answers on carries the link.
    async fn resume(&self, peer: HostId, flight: Flight) -> Result<Resuming, ChannelError> {
        let inner = self.inner()?;
        let registry = inner.channels.link_registry();
        let carrier = Arc::new(
            QuicCarrier::first_flight(
                &inner.endpoint,
                flight.paths,
                flight.through_relay,
                &inner.identity,
                inner.trust_store.clone(),
                peer,
                HANDSHAKE_BOUND,
            )
            .await
            .map_err(|error| dial_failed(&error))?,
        );
        // The carrier names the path its link came up on: the connector
        // reads it there.
        let connector_ctx = LinkConnectorCtx::new_live(inner.local_host.clone(), registry.clone())
            .with_expected_peer(peer)
            .with_incoming_streams(inner.incoming_streams_tx.clone());
        let (connector_task, established) =
            crate::routing::spawn_connector_with_establishment_and_shutdown(
                connector_ctx,
                carrier.clone(),
                inner.shutdown.subscribe(),
            );
        let abort_on_failure = AbortTaskOnDrop(connector_task.abort_handle());
        let weak = Arc::downgrade(&inner);
        drop(inner);
        let handshake = carrier.clone();
        let link_carrier = carrier.clone();
        let established = Box::pin(async move {
            if let Err(error) = handshake.handshake().await {
                return Err(dial_failed(&error));
            }
            match tokio::time::timeout(LINK_ESTABLISHMENT_TIMEOUT, established).await {
                Ok(Ok(Ok(_))) => {}
                // The host refused the identity the ticket named, or, the
                // flight not resumed, this host's certificate.
                Ok(Ok(Err(status))) if says_revoked(&status) => return Err(ChannelError::Revoked),
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
            let flown: Arc<QuicCarrier> = link_carrier.clone();
            let link = registry.link_carried_by(&flown).await.ok_or_else(|| {
                ChannelError::Handshake("a link to the host is already up".to_owned())
            })?;
            if let Some(inner) = weak.upgrade() {
                keep_link(&inner, connector_task, abort_on_failure);
            }
            Ok(link)
        });
        Ok(Resuming {
            carrier,
            established,
        })
    }

    /// A call needs a link of `peer`'s own: the core is asked to dial it,
    /// and the call waits for that attempt to end. A host that refused this
    /// one as no longer trusted, or a forwarder that refused to carry the
    /// dial, is what the call is told; otherwise it finds the link up or
    /// not.
    async fn want(&self, peer: HostId) -> Result<(), ChannelError> {
        let inner = self.inner()?;
        let registry = inner.channels.link_registry().clone();
        if registry.native_carrier_to_peer(peer).await.is_some() {
            return Ok(());
        }
        let core = registry
            .core()
            .cloned()
            .ok_or(ChannelError::LinkUnavailable { host_id: peer })?;
        let crate::driver::Wanted {
            attempt,
            revoked,
            expecting,
        } = core
            .want(core_host(peer))
            .await
            .ok_or(ChannelError::LinkUnavailable { host_id: peer })?;
        if revoked {
            return Err(ChannelError::Revoked);
        }
        let patience = tokio::time::Instant::now() + WANT_PATIENCE;
        if expecting {
            registry.link_of_peer_by(peer, patience).await;
            return Ok(());
        }
        // The core asked for the attempt before it answered, so it is out.
        // One turned away by a path is followed at once by another on the
        // rest, which the call waits for too.
        let mut attempt = attempt;
        let ended = loop {
            let over = attempt.and_then(|attempt| {
                inner
                    .attempts
                    .lock()
                    .unwrap()
                    .get(&peer)
                    .filter(|running| running.attempt == attempt)
                    .map(|running| running.over.clone())
            });
            let Some(mut over) = over else {
                break None;
            };
            let ended = tokio::time::timeout_at(patience, over.wait_for(Option::is_some))
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|ended| ended.clone());
            if !matches!(ended, Some(AttemptOutcome::TurnedAway { .. })) {
                break ended;
            }
            attempt = core
                .look(move |daemon| daemon.reach().attempt(core_host(peer)))
                .await
                .flatten();
        };
        drop(inner);
        match ended {
            Some(AttemptOutcome::Revoked) => Err(ChannelError::Revoked),
            Some(AttemptOutcome::Linked { .. }) => Ok(()),
            Some(AttemptOutcome::Failed(_) | AttemptOutcome::TurnedAway { .. }) | None => {
                match registry.last_refusal(peer) {
                    Some(reason) => Err(ChannelError::Refused(reason)),
                    None => Ok(()),
                }
            }
        }
    }

    async fn pair(
        &self,
        peer: HostId,
        through: LinkId,
        deadline: Duration,
    ) -> Result<tonic::transport::Channel, ChannelError> {
        let inner = self.inner()?;
        let registry = inner.channels.link_registry();
        let pairing = refused_or(
            registry.forward_refusals(),
            registry.as_ref(),
            peer,
            deadline,
            crate::transport::pairing_quic_channel_through(&inner.endpoint, peer, through.core()),
        )
        .await?
        .map_err(|error| ChannelError::Handshake(error.to_string()))?;
        Ok(pairing.channel)
    }
}

/// Runs `dial` under `deadline`, unless the forwarder refuses to carry
/// packets to `peer` first. A dial that times out after a
/// refusal the forwarder has not repeated yet fails with that refusal.
async fn refused_or<T>(
    mut refusals: broadcast::Receiver<(HostId, wire::pb::ForwardRefusal)>,
    registry: &crate::routing::LinkRegistry,
    peer: HostId,
    deadline: Duration,
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
        dialled = tokio::time::timeout(deadline, dial) => {
            dialled.map_err(|_| match registry.last_refusal(peer) {
                Some(reason) => ChannelError::Refused(reason),
                None => ChannelError::Handshake("the QUIC handshake timed out".to_owned()),
            })
        }
    }
}

impl ResumedDial for ForwardedDialer {
    fn dial_resumed(
        &self,
        peer: HostId,
        flight: Flight,
    ) -> BoxFuture<'_, Result<Resuming, ChannelError>> {
        Box::pin(self.resume(peer, flight))
    }
}

impl ForwardedDial for ForwardedDialer {
    fn want(&self, peer: HostId) -> BoxFuture<'_, Result<(), ChannelError>> {
        Box::pin(self.want(peer))
    }

    fn pair_through(
        &self,
        peer: HostId,
        through: LinkId,
        deadline: Duration,
    ) -> BoxFuture<'_, Result<tonic::transport::Channel, ChannelError>> {
        Box::pin(self.pair(peer, through, deadline))
    }
}

/// A dial's error, a refusal of this host's certificate read as the host
/// saying it no longer trusts this one.
fn dial_failed(error: &crate::link::quic::ConnectError) -> ChannelError {
    match error {
        _ if error.refused_as_revoked() => ChannelError::Revoked,
        crate::link::quic::ConnectError::TurnedAway { path, .. } => ChannelError::TurnedAway {
            path: *path,
            error: error.to_string(),
        },
        _ => ChannelError::Handshake(error.to_string()),
    }
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
