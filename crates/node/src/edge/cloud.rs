//! The link to the cloud relay, one attempt at a time as the core asks.
//!
//! Each attempt asks the configured cloud for relay credentials (or presents
//! the ones kept from the last), dials the relay on the carriers the core
//! chooses, runs the link until it ends, and tells the core how it ended;
//! when to try again is the core's.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use daemon_core::{AttemptId, Input, RelayCarriers, RelayOutcome};
use futures_util::FutureExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::status::{Relay, RelayCarrier, RuntimeStatus};
use super::token::{StoredToken, forget_token, load_token, store_token};
use crate::Clock;
use crate::auth::CredentialProvider;
use crate::auth::cloud::{
    CloudError, CloudRoutingConnectionDetails, fetch_routing_connection_details,
};
use crate::driver::CoreLinks;
use crate::link::QuicCarrier;
use crate::link::framed::FramedKind;
use crate::routing::{
    Host, LinkConnectorAuth, LinkConnectorRefreshReceiver, LinkConnectorRefreshRequest,
    LinkConnectorToken, LinkConnectorTokenRefresher, LinkCtx, spawn_connector,
};
use crate::transport::tls_connect_stream;

const CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);
pub const FREE_TIER_REFRESH_INTERVAL: Duration = Duration::from_secs(180);
/// How long a relay host found to drop UDP is dialled over TLS alone.
pub const UDP_BLOCKED_MEMORY: Duration = Duration::from_millis(daemon_core::reach::UDP_BLOCKED_MS);

pub struct CloudTransport {
    quic_endpoint: crate::link::LinkEndpoint,
    tcp_override: Option<std::net::SocketAddr>,
    quic_override: Option<super::RelayQuic>,
    free_refresh_interval: Option<Duration>,
}

impl CloudTransport {
    pub(crate) fn new(
        quic_endpoint: crate::link::LinkEndpoint,
        tcp_override: Option<std::net::SocketAddr>,
        quic_override: Option<super::RelayQuic>,
        free_refresh_interval: Option<Duration>,
    ) -> Self {
        Self {
            quic_endpoint,
            tcp_override,
            quic_override,
            free_refresh_interval,
        }
    }
}

pub struct CloudLink {
    stop_tx: watch::Sender<bool>,
    refresher: Option<Refresher>,
    ctx: Arc<CloudConnectionContext>,
    /// The attempt the core asked for last, while it runs.
    attempt: Mutex<Option<JoinHandle<()>>>,
}

/// The longest an entitlement refresh waits for the relay's answer. The
/// request is read only by a live connection; one made while the link is
/// between connections would otherwise wait for the next, which may never
/// come, and the phone asks right after a purchase.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Asks the relay what the account buys now, apart from the link it rides
/// on, so the asker holds nothing the link's stop needs.
#[derive(Clone)]
pub(crate) struct Refresher {
    refresh_tx: mpsc::Sender<LinkConnectorRefreshRequest>,
    status: RuntimeStatus,
}

impl Refresher {
    pub(crate) async fn refresh(&self) -> Result<crate::Tier, CloudError> {
        if !matches!(self.status.current(), Some(Relay::Connected { .. })) {
            return Err(CloudError::Connection("cloud link is not connected".into()));
        }
        let (response, response_rx) = oneshot::channel();
        // The channel holds one request. A second asker does not wait
        // behind the first, which would wait outside the timeout: it is
        // told, and the first's answer reaches the status both read.
        self.refresh_tx
            .try_send(LinkConnectorRefreshRequest { response })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    CloudError::Connection("a refresh is already under way".into())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    CloudError::Connection("cloud link is not connected".into())
                }
            })?;
        let answer = tokio::time::timeout(REFRESH_TIMEOUT, response_rx)
            .await
            .map_err(|_| {
                CloudError::Connection("the cloud did not answer the refresh in time".into())
            })?;
        let tier = answer
            .map_err(|_| CloudError::Connection("cloud link closed during refresh".into()))?
            .map_err(cloud_error_from_refresh_status)?;
        let carrier = match self.status.current() {
            Some(Relay::Connected { carrier, .. }) => carrier,
            _ => {
                return Err(CloudError::Connection(
                    "cloud link refreshed before it was connected".into(),
                ));
            }
        };
        self.status.report(Relay::Connected { tier, carrier });
        Ok(tier)
    }
}

struct CloudConnectionContext {
    cloud_url: String,
    credentials: Arc<dyn CredentialProvider>,
    /// Where the relay token is kept between launches.
    profile: PathBuf,
    clock: Arc<dyn Clock>,
    connector: LinkCtx,
    status: RuntimeStatus,
    refresh_rx: LinkConnectorRefreshReceiver,
    transport: CloudTransport,
    /// The profile's core, which decides each attempt and hears how it
    /// ended.
    core: CoreLinks,
    /// An attempt ended in a way only signing in again mends.
    finished: AtomicBool,
}

impl CloudLink {
    /// Whether an attempt ended in a way only signing in again mends.
    pub(crate) fn is_finished(&self) -> bool {
        self.ctx.finished.load(Ordering::SeqCst)
    }

    pub(crate) fn request_stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    pub(crate) async fn stop(self) {
        self.request_stop();
        let attempt = self.attempt.lock().unwrap().take();
        if let Some(attempt) = attempt {
            let _ = attempt.await;
        }
    }

    /// Ends the link at once, as a crash would: no goodbye to the relay.
    pub(crate) fn abort(self) {
        self.request_stop();
        if let Some(attempt) = self.attempt.lock().unwrap().take() {
            attempt.abort();
        }
    }

    /// What refreshes this link's entitlement, to be held across the wait
    /// instead of the link itself.
    pub(crate) fn refresher(&self) -> Option<Refresher> {
        self.refresher.clone()
    }

    /// Makes the attempt the core asked for.
    pub(crate) fn dial(&self, attempt: AttemptId) {
        let ctx = self.ctx.clone();
        let stop_rx = self.stop_tx.subscribe();
        let span = tracing::info_span!("cloud", url = %ctx.cloud_url, attempt = attempt.0);
        let task = tokio::spawn(run_attempt(ctx, stop_rx, attempt).instrument(span));
        *self.attempt.lock().unwrap() = Some(task);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn establish_cloud_link(
    cloud_url: String,
    credentials: Arc<dyn CredentialProvider>,
    profile: PathBuf,
    clock: Arc<dyn Clock>,
    connector_ctx: LinkCtx,
    status: RuntimeStatus,
    transport: CloudTransport,
    core: CoreLinks,
) -> CloudLink {
    let (stop_tx, _) = watch::channel(false);
    let (refresh_tx, refresh_rx) = mpsc::channel(1);
    let refresh_rx = Arc::new(tokio::sync::Mutex::new(refresh_rx));
    let ctx = Arc::new(CloudConnectionContext {
        cloud_url,
        credentials,
        profile,
        clock,
        connector: connector_ctx,
        status: status.clone(),
        refresh_rx,
        transport,
        core,
        finished: AtomicBool::new(false),
    });
    CloudLink {
        stop_tx,
        refresher: Some(Refresher { refresh_tx, status }),
        ctx,
        attempt: Mutex::new(None),
    }
}

/// One attempt: its dial, and its link once up, until either ends; the
/// core hears how.
async fn run_attempt(
    ctx: Arc<CloudConnectionContext>,
    stop_rx: watch::Receiver<bool>,
    attempt: AttemptId,
) {
    if *stop_rx.borrow() {
        return;
    }
    ctx.status.report(Relay::Connecting);
    tracing::info!("attempting cloud routing connection");
    let outcome = match run_cloud_connection(&ctx, stop_rx.clone(), attempt).await {
        Ok(()) => {
            tracing::info!("cloud routing connection closed cleanly");
            RelayOutcome::Closed
        }
        Err(CloudConnectionError::NonRetriable(msg)) => {
            tracing::error!(error = %msg, "cloud non-retriable error, stopping");
            ctx.finished.store(true, Ordering::SeqCst);
            RelayOutcome::Final(msg)
        }
        // The kept token is forgotten; the next attempt fetches a fresh one
        // at once, and only its refusal is final.
        Err(CloudConnectionError::KeptTokenRefused) => {
            tracing::info!("the relay refused the kept token; fetching a fresh one");
            RelayOutcome::KeptTokenRefused
        }
        Err(CloudConnectionError::Retriable { msg }) => {
            tracing::warn!(error = %msg, "cloud routing connection error, will retry");
            RelayOutcome::Lost(msg)
        }
    };
    if matches!(outcome, RelayOutcome::Closed | RelayOutcome::Lost(_)) && !*stop_rx.borrow() {
        ctx.status.report(Relay::Retrying);
    }
    ctx.core.report(Input::RelayEnded { attempt, outcome });
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable { msg: String },
    /// Error that should stop reconnection attempts.
    NonRetriable(String),
    /// The relay refused a token kept from an earlier fetch.
    KeptTokenRefused,
}

fn cloud_connection_error_from_fetch(
    error: CloudError,
    status: &RuntimeStatus,
) -> CloudConnectionError {
    match error {
        CloudError::NotAuthenticated | CloudError::Auth(_) => {
            status.report(Relay::SignInAgain);
            CloudConnectionError::NonRetriable(
                "the cloud refused this profile's credentials; sign in again".to_string(),
            )
        }
        error @ CloudError::Rejected(_) => {
            status.report(Relay::SignInAgain);
            CloudConnectionError::NonRetriable(error.to_string())
        }
        error @ CloudError::Connection(_) => CloudConnectionError::Retriable {
            msg: format!("Connection failed: {error}"),
        },
    }
}

async fn run_cloud_connection(
    ctx: &CloudConnectionContext,
    mut stop_rx: watch::Receiver<bool>,
    attempt: AttemptId,
) -> std::result::Result<(), CloudConnectionError> {
    // A stop may drop the fetch mid-refresh: the credential's exchange and
    // the save of a rotated token run apart from it and finish anyway.
    let prepared = tokio::select! {
        biased;
        _ = wait_for_stop(&mut stop_rx) => return Ok(()),
        prepared = prepare_cloud_connection(ctx) => prepared,
    };
    let prepared = prepared?;
    // The carriers are the core's to choose, now it knows the relay host.
    ctx.core
        .feed(Input::RelayPrepared {
            attempt,
            host: prepared.host.clone(),
        })
        .await;
    let carriers = ctx
        .core
        .look(move |daemon| daemon.relay_reach().carriers(attempt))
        .await
        .flatten();
    // None: the core no longer wants this attempt.
    let Some(carriers) = carriers else {
        return Ok(());
    };
    run_cloud_connection_with_details(ctx, prepared, carriers, attempt, stop_rx).await
}

/// The relay to dial and the token to present there.
struct Prepared {
    token: LinkConnectorToken,
    host: String,
    port: u16,
    /// Kept from an earlier fetch rather than fetched for this connection.
    kept: bool,
}

fn now(clock: &dyn Clock) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(clock.now_ms().max(0) as u64)
}

/// The kept token while it is valid, so a launch dials the relay without
/// waiting on the account service; otherwise a fresh one, kept for the
/// next launch.
async fn prepare_cloud_connection(
    ctx: &CloudConnectionContext,
) -> std::result::Result<Prepared, CloudConnectionError> {
    match load_token(&ctx.profile) {
        Ok(Some(kept)) if kept.usable_at(now(ctx.clock.as_ref())) => {
            tracing::info!("presenting the kept relay token");
            return Ok(Prepared {
                token: kept.connector_token(),
                host: kept.host,
                port: kept.port,
                kept: true,
            });
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "the kept relay token could not be read"),
    }
    let token = fetch_and_keep(
        ctx.cloud_url.as_str(),
        ctx.credentials.as_ref(),
        &ctx.profile,
        ctx.clock.as_ref(),
    )
    .await
    .map_err(|error| {
        if matches!(error, CloudError::NotAuthenticated | CloudError::Auth(_)) {
            tracing::warn!("the relay rejected this profile's credentials");
        }
        cloud_connection_error_from_fetch(error, &ctx.status)
    })?;
    Ok(Prepared {
        token: token.connector_token(),
        host: token.host,
        port: token.port,
        kept: false,
    })
}

/// Asks the account service for a relay token and keeps it for the next
/// launch. A token that cannot be kept is still used.
async fn fetch_and_keep(
    cloud_url: &str,
    credentials: &dyn CredentialProvider,
    profile: &std::path::Path,
    clock: &dyn Clock,
) -> std::result::Result<StoredToken, CloudError> {
    let details: CloudRoutingConnectionDetails =
        fetch_routing_connection_details(cloud_url, credentials).await?;
    let token = StoredToken {
        token: details.token,
        expires_at: SystemTime::from(details.expires_at),
        fetched_at: now(clock),
        tier: details.tier,
        host: details.host,
        port: details.port,
    };
    if let Err(error) = store_token(profile, &token) {
        tracing::warn!(%error, "the relay token could not be kept for the next launch");
    }
    Ok(token)
}

/// The relay refused the token: whatever is kept is not presented again.
fn forget_kept(ctx: &CloudConnectionContext) {
    if let Err(error) = forget_token(&ctx.profile) {
        tracing::warn!(%error, "the refused relay token could not be forgotten");
    }
}

pub(crate) type CloudDialResult = Result<Arc<QuicCarrier>, String>;

/// What a relay attempt learns of QUIC to the relay host, for the core.
#[derive(Clone)]
struct UdpFacts {
    core: CoreLinks,
    host: String,
}

impl UdpFacts {
    fn reached(&self, reached: bool) {
        self.core.report(Input::RelayUdp {
            host: self.host.clone(),
            reached,
        });
    }
}

/// Dials the relay on `carriers`: TLS alone, or QUIC with TLS after its
/// head start, the first to link kept. What QUIC's dial came to, even one
/// still out once TLS won, is told the core.
async fn select_cloud_carrier<Q, T>(
    carriers: RelayCarriers,
    udp: UdpFacts,
    quic: Q,
    tcp: T,
) -> Result<(Arc<QuicCarrier>, RelayCarrier), String>
where
    Q: Future<Output = CloudDialResult> + Send + 'static,
    T: Future<Output = CloudDialResult> + Send,
{
    let head_start = match carriers {
        RelayCarriers::Tcp => {
            tracing::debug!(host = %udp.host, "remembered UDP-blocked cloud host; dialing TCP only");
            return tcp.await.map(|carrier| (carrier, RelayCarrier::Tcp));
        }
        RelayCarriers::Both { head_start_ms } => Duration::from_millis(head_start_ms),
    };

    let mut quic = Box::pin(quic);
    let tcp = async move {
        tokio::time::sleep(head_start).await;
        tcp.await
    };
    tokio::pin!(tcp);

    enum First {
        Quic(CloudDialResult),
        Tcp(CloudDialResult),
    }
    let first = tokio::select! {
        biased;
        result = quic.as_mut() => First::Quic(result),
        result = &mut tcp => First::Tcp(result),
    };

    match first {
        First::Quic(Ok(carrier)) => {
            close_ready_loser(tcp.as_mut().now_or_never());
            udp.reached(true);
            Ok((carrier, RelayCarrier::Quic))
        }
        First::Tcp(Ok(carrier)) => {
            match quic.as_mut().now_or_never() {
                Some(Ok(late_quic)) => {
                    late_quic.close(wire::LinkCloseReason::UserShutdown);
                    udp.reached(true);
                }
                Some(Err(error)) => {
                    tracing::debug!(%error, "cloud QUIC dial failed after all candidates");
                    udp.reached(false);
                }
                None => {
                    tokio::spawn(async move {
                        match quic.await {
                            Ok(late_quic) => {
                                late_quic.close(wire::LinkCloseReason::UserShutdown);
                                udp.reached(true);
                            }
                            Err(error) => {
                                tracing::debug!(%error, host = %udp.host, "cloud QUIC probe failed after all candidates");
                                udp.reached(false);
                            }
                        }
                    });
                }
            }
            Ok((carrier, RelayCarrier::Tcp))
        }
        First::Quic(Err(quic_error)) => {
            tracing::debug!(error = %quic_error, "cloud QUIC dial failed; waiting for TCP");
            match tcp.await {
                Ok(carrier) => {
                    udp.reached(false);
                    Ok((carrier, RelayCarrier::Tcp))
                }
                Err(tcp_error) => Err(format!(
                    "QUIC failed: {quic_error}; TCP failed: {tcp_error}"
                )),
            }
        }
        First::Tcp(Err(tcp_error)) => {
            tracing::debug!(error = %tcp_error, "cloud TCP dial failed; waiting for QUIC");
            match quic.await {
                Ok(carrier) => {
                    udp.reached(true);
                    Ok((carrier, RelayCarrier::Quic))
                }
                Err(quic_error) => Err(format!(
                    "QUIC failed: {quic_error}; TCP failed: {tcp_error}"
                )),
            }
        }
    }
}

fn close_ready_loser(result: Option<CloudDialResult>) {
    if let Some(Ok(carrier)) = result {
        carrier.close(wire::LinkCloseReason::UserShutdown);
    }
}

async fn run_cloud_connection_with_details(
    ctx: &CloudConnectionContext,
    details: Prepared,
    carriers: RelayCarriers,
    attempt: AttemptId,
    stop_rx: watch::Receiver<bool>,
) -> std::result::Result<(), CloudConnectionError> {
    tracing::info!(host = %details.host, port = details.port, kept = details.kept, "connecting to cloud routing");
    // One configuration for both carriers, kept by the profile's endpoint
    // so the ticket the relay's last link left (in this process or the
    // last) is offered and the Hello goes as early data.
    let quic_override = ctx.transport.quic_override.clone();
    let roots = match &quic_override {
        Some(relay) => Ok(relay.roots.clone()),
        None => crate::transport::cloud_root_store(),
    };
    let client = roots
        .and_then(|roots| {
            ctx.transport
                .quic_endpoint
                .relay_config(&details.host, roots)
        })
        .map_err(|error| CloudConnectionError::Retriable {
            msg: format!("Connection failed: {error}"),
        })?;
    let quic_endpoint = ctx.transport.quic_endpoint.clone();
    let quic_host = details.host.clone();
    let quic_port = details.port;
    let quic_client = client.clone();
    // The dial writes the Hello itself on each address it tries, so a
    // resumed one says it in its first flight and still wins only once the
    // relay answers: QUIC's early data never outruns the TCP fallback.
    // The connection carries data before the relay admits it, so links'
    // first flights go through it at once: the relay holds what they send
    // until it has. Each attempt takes them as it says its Hello, and the
    // one kept takes them once the dial settles; once admitted, the link
    // is named afresh.
    let socket = ctx.transport.quic_endpoint.socket.clone();
    let link = crate::routing::LinkId::new(crate::HostId::nil()).core();
    let settled = Arc::new(Mutex::new(false));
    let _forwarding = Unforward(socket.clone(), Some(link));
    let opening = crate::link::run::relay_opening(
        &ctx.connector,
        &details.token.token,
        Some(crate::link::quic::EarlyForwarding {
            link,
            settled: settled.clone(),
        }),
    )
    .await;
    let quic = async move {
        #[cfg(debug_assertions)]
        if std::env::var_os("AMUX_TEST_RELAY_UDP_BLOCKED").is_some() {
            return Err("relay UDP blocked by test fixture".to_string());
        }
        match quic_override {
            Some(relay) => {
                QuicCarrier::connect_relay_candidates_with_config(
                    &quic_endpoint,
                    [relay.addr],
                    &quic_host,
                    quic_port,
                    quic_client,
                    Some(&opening),
                )
                .await
            }
            None => {
                QuicCarrier::connect_relay(
                    &quic_endpoint,
                    &quic_host,
                    quic_port,
                    quic_client,
                    Some(&opening),
                )
                .await
            }
        }
        .map(Arc::new)
        .map_err(|error| error.to_string())
    };
    let tcp_host = details.host.clone();
    let tcp_port = details.port;
    let fixture_transport = ctx.transport.tcp_override;
    let tcp_endpoint = ctx.transport.quic_endpoint.clone();
    let tcp = async move {
        #[cfg(debug_assertions)]
        if std::env::var_os("AMUX_TEST_RELAY_TCP_BLOCKED").is_some() {
            return Err("relay TCP blocked by test fixture".to_string());
        }
        let stream = cloud_routing_stream(&tcp_host, tcp_port, fixture_transport)
            .await
            .map_err(|error| error.to_string())?;
        QuicCarrier::connect_framed(
            &tcp_endpoint,
            stream,
            FramedKind::RelayTcp,
            client,
            &tcp_host,
        )
        .await
        .map(Arc::new)
        .map_err(|error| error.to_string())
    };
    let (carrier, relay_carrier) = tokio::time::timeout(
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        select_cloud_carrier(
            carriers,
            UdpFacts {
                core: ctx.core.clone(),
                host: details.host.clone(),
            },
            quic,
            tcp,
        ),
    )
    .await
    .map_err(|_| CloudConnectionError::Retriable {
        msg: "cloud routing transport dial timed out".to_string(),
    })?
    .map_err(|error| CloudConnectionError::Retriable {
        msg: format!("Connection failed: {error}"),
    })?;
    {
        let mut settled = settled.lock().unwrap_or_else(|e| e.into_inner());
        *settled = true;
        match carrier.remote_address() {
            Some(via) => socket.add_forwarding(link, via, carrier.path_kind()),
            None => socket.remove_forwarding(link),
        }
    }
    let tier = details.token.tier;
    let kept = details.kept;
    let refresh_status = ctx.status.clone();
    let connector_auth = LinkConnectorAuth::with_free_refresh_interval(
        details.token,
        Arc::new(CloudLinkTokenRefresher {
            cloud_url: ctx.cloud_url.clone(),
            credentials: ctx.credentials.clone(),
            profile: ctx.profile.clone(),
            clock: ctx.clock.clone(),
            current_host: details.host,
            current_port: details.port,
        }),
        Some(
            ctx.transport
                .free_refresh_interval
                .unwrap_or(FREE_TIER_REFRESH_INTERVAL),
        ),
    )
    .with_refresh_observer(move |tier| {
        refresh_status.report(Relay::Connected {
            tier,
            carrier: relay_carrier,
        });
    });
    let (connector_task, established_rx) = spawn_connector(
        ctx.connector
            .clone()
            .with_connector_auth(connector_auth, Some(ctx.refresh_rx.clone()))
            .with_shutdown(stop_rx),
        carrier,
    );
    let _abort_connector_on_drop = AbortTaskOnDrop(connector_task.abort_handle());
    let established = await_cloud_establishment(
        &ctx.status,
        established_rx,
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        tier,
        relay_carrier,
        kept,
    )
    .await;
    if matches!(established, Err(CloudConnectionError::KeptTokenRefused)) {
        forget_kept(ctx);
    }
    established?;
    ctx.core.report(Input::RelayLinked { attempt });

    let result = connector_task
        .await
        .map_err(|error| CloudConnectionError::Retriable {
            msg: format!("cloud routing task failed: {error}"),
        })?;

    match result {
        Ok(()) => Ok(()),
        // The relay took these credentials when the link came up, so turning
        // them away now means they lapsed first: a refresh lost the race with
        // the relay's clock, or the host slept past the expiry. A new
        // connection fetches fresh ones; credentials that are really gone
        // fail there and ask for a sign-in.
        Err(error) if error.code() == tonic::Code::Unauthenticated => {
            forget_kept(ctx);
            Err(CloudConnectionError::Retriable {
                msg: error.to_string(),
            })
        }
        Err(error) => Err(cloud_connection_error_from_status(&ctx.status, error).await),
    }
}

/// Forgets the forwarding link a relay connection was before the relay
/// admitted it, when the connection ends; one named afresh on admission
/// is not this one any more.
struct Unforward(
    Arc<crate::link::socket::LinkSocket>,
    Option<crate::link::socket::LinkId>,
);

impl Drop for Unforward {
    fn drop(&mut self) {
        if let Some(link) = self.1 {
            self.0.remove_forwarding(link);
        }
    }
}

async fn wait_for_stop(stop_rx: &mut watch::Receiver<bool>) {
    loop {
        if *stop_rx.borrow() {
            return;
        }
        match stop_rx.changed().await {
            Ok(()) => {}
            // The handle went with the link that owned it: nothing is left
            // to stop this task, and a link that dialled on with no owner
            // would hold the profile's endpoint, and so its port, for good.
            Err(_) => return,
        }
    }
}

async fn await_cloud_establishment(
    observed: &RuntimeStatus,
    established_rx: oneshot::Receiver<Result<Host, tonic::Status>>,
    timeout: Duration,
    tier: crate::Tier,
    carrier: RelayCarrier,
    kept: bool,
) -> std::result::Result<(), CloudConnectionError> {
    match tokio::time::timeout(timeout, established_rx).await {
        Ok(Ok(Ok(_))) => {
            observed.report(Relay::Connected { tier, carrier });
            Ok(())
        }
        Ok(Ok(Err(status))) if kept && status.code() == tonic::Code::Unauthenticated => {
            Err(CloudConnectionError::KeptTokenRefused)
        }
        Ok(Ok(Err(status))) => Err(cloud_connection_error_from_status(observed, status).await),
        Ok(Err(_)) => Ok(()),
        Err(_) => Err(CloudConnectionError::Retriable {
            msg: "cloud routing handshake timed out".to_string(),
        }),
    }
}

async fn cloud_connection_error_from_status(
    observed: &RuntimeStatus,
    status: tonic::Status,
) -> CloudConnectionError {
    if crate::net_error::from_status(&status)
        .is_some_and(|error| crate::net_error::is_version_mismatch(&error))
    {
        observed.report(Relay::VersionMismatch);
        return CloudConnectionError::NonRetriable(status.to_string());
    }
    if status.code() == tonic::Code::Unauthenticated {
        observed.report(Relay::SignInAgain);
        tracing::warn!(%status, "the relay refused this profile's credentials");
        return CloudConnectionError::NonRetriable(
            "the relay refused this profile's credentials; sign in again".to_string(),
        );
    }
    CloudConnectionError::Retriable {
        msg: status.to_string(),
    }
}

trait CloudIo: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T> CloudIo for T where T: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

type BoxedCloudIo = Box<dyn CloudIo>;

async fn cloud_routing_stream(
    host: &str,
    port: u16,
    transport: Option<std::net::SocketAddr>,
) -> crate::transport::Result<BoxedCloudIo> {
    if let Some(address) = transport {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        return Ok(Box::new(stream));
    }
    Ok(Box::new(tls_connect_stream(host, port).await?))
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct CloudLinkTokenRefresher {
    cloud_url: String,
    credentials: Arc<dyn CredentialProvider>,
    profile: PathBuf,
    clock: Arc<dyn Clock>,
    current_host: String,
    current_port: u16,
}

fn cloud_token_refresh_status(error: CloudError) -> tonic::Status {
    match error {
        CloudError::NotAuthenticated | CloudError::Auth(_) => {
            tonic::Status::unauthenticated("invalid cloud credentials")
        }
        CloudError::Rejected(message) => tonic::Status::permission_denied(message),
        CloudError::Connection(message) => tonic::Status::unavailable(message),
    }
}

fn cloud_error_from_refresh_status(status: tonic::Status) -> CloudError {
    match status.code() {
        tonic::Code::Unauthenticated => CloudError::Auth(status.message().to_string()),
        tonic::Code::PermissionDenied => CloudError::Rejected(status.message().to_string()),
        _ => CloudError::Connection(status.message().to_string()),
    }
}

#[tonic::async_trait]
impl LinkConnectorTokenRefresher for CloudLinkTokenRefresher {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        // Kept before the endpoint is compared: a relay that moved is the
        // one the next connection dials.
        let token = fetch_and_keep(
            &self.cloud_url,
            self.credentials.as_ref(),
            &self.profile,
            self.clock.as_ref(),
        )
        .await
        .map_err(cloud_token_refresh_status)?;

        if token.host != self.current_host || token.port != self.current_port {
            return Err(tonic::Status::unavailable(
                "cloud routing endpoint changed during reauth",
            ));
        }

        Ok(token.connector_token())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresher(relay: Relay) -> (Refresher, mpsc::Receiver<LinkConnectorRefreshRequest>) {
        let (refresh_tx, refresh_rx) = mpsc::channel(1);
        let status = RuntimeStatus::default();
        status.report(relay);
        (Refresher { refresh_tx, status }, refresh_rx)
    }

    #[tokio::test]
    async fn a_refresh_between_connections_fails_at_once() {
        let (refresher, _rx) = refresher(Relay::Retrying);
        let error = refresher.refresh().await.unwrap_err();
        assert!(error.to_string().contains("not connected"), "{error}");
    }

    #[tokio::test]
    async fn a_second_refresh_behind_an_unanswered_one_is_told_at_once() {
        let (refresher, _rx) = refresher(Relay::Connected {
            tier: crate::Tier::Free,
            carrier: RelayCarrier::Quic,
        });
        let first = tokio::spawn({
            let refresher = refresher.clone();
            async move { refresher.refresh().await }
        });
        tokio::task::yield_now().await;
        let error = refresher.refresh().await.unwrap_err();
        assert!(error.to_string().contains("already under way"), "{error}");
        first.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_refresh_nobody_answers_ends_at_the_timeout() {
        let (refresher, _rx) = refresher(Relay::Connected {
            tier: crate::Tier::Free,
            carrier: RelayCarrier::Quic,
        });
        let started = tokio::time::Instant::now();
        let error = refresher.refresh().await.unwrap_err();
        assert!(error.to_string().contains("in time"), "{error}");
        assert_eq!(started.elapsed(), REFRESH_TIMEOUT);
    }
}
