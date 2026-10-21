//! Cloud relay connection with automatic reconnection.
//!
//! Asks the configured cloud for relay credentials, then connects to that relay.
//! Handles exponential backoff on retriable errors and stops on auth failures.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures_util::FutureExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;
use uuid::Uuid;

use super::status::{Observed, RelayCarrier, RuntimeStatus};
use crate::auth::CredentialProvider;
use crate::auth::cloud::{
    CloudError, CloudRoutingConnectionDetails, fetch_routing_connection_details,
};
use crate::link::framed::{FramedKind, StreamEndpoint};
use crate::link::{LinkCarrier, QuicCarrier};
use crate::routing::{
    Host, LinkConnectorAuth, LinkConnectorCtx, LinkConnectorRefreshReceiver,
    LinkConnectorRefreshRequest, LinkConnectorToken, LinkConnectorTokenRefresher,
    spawn_connector_with_auth_establishment_and_shutdown,
};
use crate::transport::tls_connect_stream;
use crate::{Clock, audit};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const RELATIVE_JITTER_RATIO: f64 = 0.25;
const ABSOLUTE_JITTER_MAX: Duration = Duration::from_secs(5);
const BACKOFF_RESET_AFTER_ESTABLISHED: Duration = Duration::from_secs(30);
const CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(10);
pub const FREE_TIER_REFRESH_INTERVAL: Duration = Duration::from_secs(180);
pub const UDP_BLOCKED_MEMORY: Duration = Duration::from_secs(3600);
pub(crate) const TCP_FALLBACK_DELAY: Duration = Duration::from_millis(300);

pub struct UdpBlockedMemory {
    duration: Duration,
    clock: Arc<dyn Clock>,
    /// When each host was found to eat UDP, on the policy clock.
    blocked_at: Mutex<HashMap<String, i64>>,
    changed: watch::Sender<u64>,
}

pub struct CloudTransport {
    quic_endpoint: crate::link::LinkEndpoint,
    udp_blocked: Arc<UdpBlockedMemory>,
    tcp_override: Option<std::net::SocketAddr>,
    quic_override: Option<super::RelayQuic>,
    free_refresh_interval: Option<Duration>,
}

impl CloudTransport {
    pub(crate) fn new(
        quic_endpoint: crate::link::LinkEndpoint,
        udp_blocked: Arc<UdpBlockedMemory>,
        tcp_override: Option<std::net::SocketAddr>,
        quic_override: Option<super::RelayQuic>,
        free_refresh_interval: Option<Duration>,
    ) -> Self {
        Self {
            quic_endpoint,
            udp_blocked,
            tcp_override,
            quic_override,
            free_refresh_interval,
        }
    }
}

impl UdpBlockedMemory {
    pub(crate) fn new(duration: Duration, clock: Arc<dyn Clock>) -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            duration,
            clock,
            blocked_at: Mutex::new(HashMap::new()),
            changed,
        }
    }

    pub(crate) fn holds(&self, host: &str) -> bool {
        let mut blocked_at = self
            .blocked_at
            .lock()
            .expect("UDP-blocked memory lock poisoned");
        let now = self.clock.now_ms();
        let previous_len = blocked_at.len();
        let memory = self.duration.as_millis() as i64;
        blocked_at.retain(|_, recorded| now - *recorded < memory);
        if blocked_at.len() != previous_len {
            self.changed.send_modify(|generation| *generation += 1);
        }
        blocked_at.contains_key(host)
    }

    fn record(&self, host: &str) {
        self.blocked_at
            .lock()
            .expect("UDP-blocked memory lock poisoned")
            .insert(host.to_string(), self.clock.now_ms());
        self.changed.send_modify(|generation| *generation += 1);
    }

    fn clear(&self, host: &str) {
        let removed = self
            .blocked_at
            .lock()
            .expect("UDP-blocked memory lock poisoned")
            .remove(host)
            .is_some();
        if removed {
            self.changed.send_modify(|generation| *generation += 1);
        }
    }
}

#[allow(dead_code)]
pub struct CloudLink {
    stop_tx: watch::Sender<bool>,
    refresher: Option<Refresher>,
    status: RuntimeStatus,
    task: JoinHandle<()>,
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
        if !matches!(self.status.current(), Observed::Connected { .. }) {
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
            Observed::Connected { carrier, .. } => carrier,
            _ => {
                return Err(CloudError::Connection(
                    "cloud link refreshed before it was connected".into(),
                ));
            }
        };
        self.status.report(Observed::Connected { tier, carrier });
        Ok(tier)
    }
}

struct CloudConnectionContext {
    cloud_url: String,
    credentials: Arc<dyn CredentialProvider>,
    connector: LinkConnectorCtx,
    status: RuntimeStatus,
    refresh_rx: LinkConnectorRefreshReceiver,
    transport: CloudTransport,
}

impl CloudLink {
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(crate) fn request_stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    pub(crate) async fn stop(self) {
        self.request_stop();
        let _ = self.task.await;
    }

    /// Ends the link at once, as a crash would: no goodbye to the relay.
    pub(crate) fn abort(self) {
        self.request_stop();
        self.task.abort();
    }

    /// What refreshes this link's entitlement, to be held across the wait
    /// instead of the link itself.
    pub(crate) fn refresher(&self) -> Option<Refresher> {
        self.refresher.clone()
    }
}

pub(crate) fn establish_cloud_link(
    cloud_url: String,
    credentials: Arc<dyn CredentialProvider>,
    connector_ctx: LinkConnectorCtx,
    status: RuntimeStatus,
    transport: CloudTransport,
) -> CloudLink {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let (refresh_tx, refresh_rx) = mpsc::channel(1);
    let refresh_rx = Arc::new(tokio::sync::Mutex::new(refresh_rx));
    let task_status = status.clone();
    let cloud_span = tracing::info_span!("cloud", url = %cloud_url);
    let connection = CloudConnectionContext {
        cloud_url,
        credentials,
        connector: connector_ctx,
        status: task_status,
        refresh_rx,
        transport,
    };
    let task = tokio::spawn(
        async move {

            let mut backoff = INITIAL_BACKOFF;

            loop {
                if *stop_rx.borrow() {
                    return;
                }
                connection.status.report(Observed::Connecting);
                tracing::info!("attempting cloud routing connection");
                match run_cloud_connection(&connection, stop_rx.clone()).await
                {
                    Ok(()) => {
                        tracing::info!("cloud routing connection closed cleanly");
                        backoff = INITIAL_BACKOFF;
                    }
                    Err(CloudConnectionError::NonRetriable(msg)) => {
                        tracing::error!(error = %msg, "cloud non-retriable error, stopping");
                        return;
                    }
                    Err(CloudConnectionError::Retriable { msg, reset_backoff }) => {
                        if reset_backoff {
                            backoff = INITIAL_BACKOFF;
                        }
                        tracing::warn!(error = %msg, "cloud routing connection error, will retry");
                    }
                }

                if *stop_rx.borrow() {
                    return;
                }
                connection.status.report(Observed::Retrying);

                let retry_delay = jittered_backoff(backoff);
                tracing::info!(base_backoff = ?backoff, retry_delay = ?retry_delay, "reconnecting to cloud");
                if sleep_or_stop(retry_delay, &mut stop_rx).await {
                    return;
                }
                backoff = next_backoff(backoff);
            }
        }
        .instrument(cloud_span),
    );
    CloudLink {
        stop_tx,
        refresher: Some(Refresher {
            refresh_tx,
            status: status.clone(),
        }),
        status,
        task,
    }
}

/// Error type for cloud connection attempts
enum CloudConnectionError {
    /// Error that should trigger reconnection (connection lost, host changed)
    Retriable { msg: String, reset_backoff: bool },
    /// Error that should stop reconnection attempts.
    NonRetriable(String),
}

fn cloud_connection_error_from_fetch(
    error: CloudError,
    status: &RuntimeStatus,
) -> CloudConnectionError {
    match error {
        CloudError::NotAuthenticated | CloudError::Auth(_) => {
            status.report(Observed::AuthenticationRequired);
            CloudConnectionError::NonRetriable(
                "the cloud refused this profile's credentials; sign in again".to_string(),
            )
        }
        error @ CloudError::Rejected(_) => {
            status.report(Observed::AuthenticationRequired);
            CloudConnectionError::NonRetriable(error.to_string())
        }
        error @ CloudError::Connection(_) => CloudConnectionError::Retriable {
            msg: format!("Connection failed: {error}"),
            reset_backoff: false,
        },
    }
}

async fn run_cloud_connection(
    ctx: &CloudConnectionContext,
    mut stop_rx: watch::Receiver<bool>,
) -> std::result::Result<(), CloudConnectionError> {
    // A stop may drop the fetch mid-refresh: the credential's exchange and
    // the save of a rotated token run apart from it and finish anyway.
    let prepared = tokio::select! {
        biased;
        _ = wait_for_stop(&mut stop_rx) => return Ok(()),
        prepared = prepare_cloud_connection(&ctx.cloud_url, &ctx.credentials, &ctx.status) => prepared,
    };
    let (credentials, details) = prepared?;

    run_cloud_connection_with_details(ctx, credentials, details, stop_rx).await
}

async fn prepare_cloud_connection(
    cloud_url: &str,
    credentials: &Arc<dyn CredentialProvider>,
    status: &RuntimeStatus,
) -> std::result::Result<
    (Arc<dyn CredentialProvider>, CloudRoutingConnectionDetails),
    CloudConnectionError,
> {
    let credentials = credentials.clone();
    let details = match fetch_routing_connection_details(cloud_url, credentials.as_ref()).await {
        Ok(details) => details,
        Err(error) => {
            if matches!(error, CloudError::NotAuthenticated | CloudError::Auth(_)) {
                audit::auth_jwt_failure("cloud routing credentials were rejected");
            }
            return Err(cloud_connection_error_from_fetch(error, status));
        }
    };

    Ok((credentials, details))
}

pub(crate) type CloudDialResult = Result<Arc<dyn LinkCarrier>, String>;

pub(crate) async fn select_cloud_carrier<Q, T>(
    host: &str,
    udp_blocked: Arc<UdpBlockedMemory>,
    fallback_delay: Duration,
    quic: Q,
    tcp: T,
) -> Result<(Arc<dyn LinkCarrier>, RelayCarrier), String>
where
    Q: Future<Output = CloudDialResult> + Send + 'static,
    T: Future<Output = CloudDialResult> + Send,
{
    if udp_blocked.holds(host) {
        tracing::debug!(host, "remembered UDP-blocked cloud host; dialing TCP only");
        return tcp.await.map(|carrier| (carrier, RelayCarrier::Tcp));
    }

    let mut quic = Box::pin(quic);
    let tcp = async move {
        tokio::time::sleep(fallback_delay).await;
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
            udp_blocked.clear(host);
            Ok((carrier, RelayCarrier::Quic))
        }
        First::Tcp(Ok(carrier)) => {
            match quic.as_mut().now_or_never() {
                Some(Ok(late_quic)) => {
                    late_quic.close(wire::pb::LinkCloseReason::UserShutdown);
                    udp_blocked.clear(host);
                }
                Some(Err(error)) => {
                    tracing::debug!(%error, "cloud QUIC dial failed after all candidates");
                    udp_blocked.record(host);
                }
                None => {
                    let host = host.to_string();
                    tokio::spawn(async move {
                        match quic.await {
                            Ok(late_quic) => {
                                late_quic.close(wire::pb::LinkCloseReason::UserShutdown);
                                udp_blocked.clear(&host);
                            }
                            Err(error) => {
                                tracing::debug!(%error, %host, "cloud QUIC probe failed after all candidates");
                                udp_blocked.record(&host);
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
                    udp_blocked.record(host);
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
                    udp_blocked.clear(host);
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
        carrier.close(wire::pb::LinkCloseReason::UserShutdown);
    }
}

async fn run_cloud_connection_with_details(
    ctx: &CloudConnectionContext,
    credentials: Arc<dyn CredentialProvider>,
    details: CloudRoutingConnectionDetails,
    stop_rx: watch::Receiver<bool>,
) -> std::result::Result<(), CloudConnectionError> {
    tracing::info!(host = %details.host, port = details.port, "connecting to cloud routing");
    let quic_endpoint = ctx.transport.quic_endpoint.clone();
    let quic_host = details.host.clone();
    let quic_port = details.port;
    let quic_override = ctx.transport.quic_override.clone();
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
                    relay.client,
                )
                .await
            }
            None => QuicCarrier::connect_relay(&quic_endpoint, &quic_host, quic_port).await,
        }
        .map(|carrier| Arc::new(carrier) as Arc<dyn LinkCarrier>)
        .map_err(|error| error.to_string())
    };
    let tcp_host = details.host.clone();
    let tcp_port = details.port;
    let fixture_transport = ctx.transport.tcp_override;
    let tcp_client = ctx
        .transport
        .quic_override
        .as_ref()
        .map(|relay| relay.client.clone());
    let tcp = async move {
        let stream = cloud_routing_stream(&tcp_host, tcp_port, fixture_transport)
            .await
            .map_err(|error| error.to_string())?;
        let stream = StreamEndpoint::new(stream, FramedKind::RelayTcp, None)
            .map_err(|error| error.to_string())?;
        let client = match tcp_client {
            Some(client) => client,
            None => {
                crate::transport::relay_quic_client_config().map_err(|error| error.to_string())?
            }
        };
        QuicCarrier::connect_over_stream(stream, client, &tcp_host)
            .await
            .map(|carrier| Arc::new(carrier) as Arc<dyn LinkCarrier>)
            .map_err(|error| error.to_string())
    };
    let (carrier, relay_carrier) = tokio::time::timeout(
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        select_cloud_carrier(
            &details.host,
            ctx.transport.udp_blocked.clone(),
            TCP_FALLBACK_DELAY,
            quic,
            tcp,
        ),
    )
    .await
    .map_err(|_| CloudConnectionError::Retriable {
        msg: "cloud routing transport dial timed out".to_string(),
        reset_backoff: false,
    })?
    .map_err(|error| CloudConnectionError::Retriable {
        msg: format!("Connection failed: {error}"),
        reset_backoff: false,
    })?;
    let connected_at = std::time::Instant::now();
    let tier = details.tier;
    let refresh_status = ctx.status.clone();
    let connector_auth = LinkConnectorAuth::with_free_refresh_interval(
        LinkConnectorToken {
            token: details.token,
            expires_at: SystemTime::from(details.expires_at),
            tier,
        },
        Arc::new(CloudLinkTokenRefresher {
            cloud_url: ctx.cloud_url.clone(),
            credentials,
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
        refresh_status.report(Observed::Connected {
            tier,
            carrier: relay_carrier,
        });
    });
    let (connector_task, established_rx) = spawn_connector_with_auth_establishment_and_shutdown(
        ctx.connector.clone(),
        carrier,
        connector_auth,
        stop_rx,
        Some(ctx.refresh_rx.clone()),
    );
    let _abort_connector_on_drop = AbortTaskOnDrop(connector_task.abort_handle());
    await_cloud_establishment(
        &ctx.status,
        established_rx,
        connected_at,
        CLOUD_ROUTING_ESTABLISHMENT_TIMEOUT,
        tier,
        relay_carrier,
    )
    .await?;

    let result = connector_task
        .await
        .map_err(|error| CloudConnectionError::Retriable {
            msg: format!("cloud routing task failed: {error}"),
            reset_backoff: should_reset_backoff_after_connection(connected_at.elapsed()),
        })?;

    match result {
        Ok(()) => Ok(()),
        // The relay took these credentials when the link came up, so turning
        // them away now means they lapsed first: a refresh lost the race with
        // the relay's clock, or the host slept past the expiry. A new
        // connection fetches fresh ones; credentials that are really gone
        // fail there and ask for a sign-in.
        Err(error) if error.code() == tonic::Code::Unauthenticated => {
            Err(CloudConnectionError::Retriable {
                msg: error.to_string(),
                reset_backoff: should_reset_backoff_after_connection(connected_at.elapsed()),
            })
        }
        Err(error) => {
            Err(
                cloud_connection_error_from_status(&ctx.status, error, connected_at.elapsed())
                    .await,
            )
        }
    }
}

async fn sleep_or_stop(duration: Duration, stop_rx: &mut watch::Receiver<bool>) -> bool {
    if *stop_rx.borrow() {
        return true;
    }

    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    tokio::select! {
        biased;
        _ = wait_for_stop(stop_rx) => true,
        _ = &mut sleep => false,
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
    connected_at: std::time::Instant,
    timeout: Duration,
    tier: crate::Tier,
    carrier: RelayCarrier,
) -> std::result::Result<(), CloudConnectionError> {
    match tokio::time::timeout(timeout, established_rx).await {
        Ok(Ok(Ok(_))) => {
            observed.report(Observed::Connected { tier, carrier });
            Ok(())
        }
        Ok(Ok(Err(status))) => {
            Err(cloud_connection_error_from_status(observed, status, connected_at.elapsed()).await)
        }
        Ok(Err(_)) => Ok(()),
        Err(_) => Err(CloudConnectionError::Retriable {
            msg: "cloud routing handshake timed out".to_string(),
            reset_backoff: false,
        }),
    }
}

async fn cloud_connection_error_from_status(
    observed: &RuntimeStatus,
    status: tonic::Status,
    connection_uptime: Duration,
) -> CloudConnectionError {
    if crate::net_error::from_status(&status)
        .is_some_and(|error| crate::net_error::is_version_mismatch(&error))
    {
        observed.report(Observed::VersionMismatch);
        return CloudConnectionError::NonRetriable(status.to_string());
    }
    if status.code() == tonic::Code::Unauthenticated {
        observed.report(Observed::AuthenticationRequired);
        audit::auth_jwt_failure(&status);
        return CloudConnectionError::NonRetriable(
            "the relay refused this profile's credentials; sign in again".to_string(),
        );
    }
    CloudConnectionError::Retriable {
        msg: status.to_string(),
        reset_backoff: should_reset_backoff_after_connection(connection_uptime),
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
        crate::transport::configure_relay_tcp_keepalive(&stream);
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
        let details = fetch_routing_connection_details(&self.cloud_url, self.credentials.as_ref())
            .await
            .map_err(cloud_token_refresh_status)?;

        if details.host != self.current_host || details.port != self.current_port {
            return Err(tonic::Status::unavailable(
                "cloud routing endpoint changed during reauth",
            ));
        }

        Ok(LinkConnectorToken {
            token: details.token,
            expires_at: SystemTime::from(details.expires_at),
            tier: details.tier,
        })
    }
}

fn next_backoff(backoff: Duration) -> Duration {
    std::cmp::min(backoff * 2, MAX_BACKOFF)
}

fn jittered_backoff(base_backoff: Duration) -> Duration {
    jittered_backoff_with_samples(base_backoff, random_unit_interval(), random_unit_interval())
}

fn jittered_backoff_with_samples(
    base_backoff: Duration,
    relative_sample: f64,
    absolute_sample: f64,
) -> Duration {
    debug_assert!((0.0..=1.0).contains(&relative_sample));
    debug_assert!((0.0..=1.0).contains(&absolute_sample));

    let base_secs = base_backoff.as_secs_f64();
    let relative_offset = base_secs * RELATIVE_JITTER_RATIO * ((relative_sample * 2.0) - 1.0);
    let absolute_offset = ABSOLUTE_JITTER_MAX.as_secs_f64() * absolute_sample;
    Duration::from_secs_f64((base_secs + relative_offset + absolute_offset).max(0.0))
}

fn random_unit_interval() -> f64 {
    let uuid = Uuid::new_v4();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&uuid.as_bytes()[..8]);
    let sample = u64::from_le_bytes(bytes);
    sample as f64 / u64::MAX as f64
}

fn should_reset_backoff_after_connection(connection_uptime: Duration) -> bool {
    connection_uptime >= BACKOFF_RESET_AFTER_ESTABLISHED
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresher(observed: Observed) -> (Refresher, mpsc::Receiver<LinkConnectorRefreshRequest>) {
        let (refresh_tx, refresh_rx) = mpsc::channel(1);
        let status = RuntimeStatus::default();
        status.report(observed);
        (Refresher { refresh_tx, status }, refresh_rx)
    }

    #[tokio::test]
    async fn a_refresh_between_connections_fails_at_once() {
        let (refresher, _rx) = refresher(Observed::Retrying);
        let error = refresher.refresh().await.unwrap_err();
        assert!(error.to_string().contains("not connected"), "{error}");
    }

    #[tokio::test]
    async fn a_second_refresh_behind_an_unanswered_one_is_told_at_once() {
        let (refresher, _rx) = refresher(Observed::Connected {
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
        let (refresher, _rx) = refresher(Observed::Connected {
            tier: crate::Tier::Free,
            carrier: RelayCarrier::Quic,
        });
        let started = tokio::time::Instant::now();
        let error = refresher.refresh().await.unwrap_err();
        assert!(error.to_string().contains("in time"), "{error}");
        assert_eq!(started.elapsed(), REFRESH_TIMEOUT);
    }
}
