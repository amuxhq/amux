use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustls::pki_types::CertificateDer;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::driver::pairing::PairWindow as PairMode;
use crate::identity::{self, DeviceIdentity, IdentityError};
use crate::link::{InboundStream, LinkCtx, QuicCarrier, accepted_quic_bidi_stream, run_link};
use crate::resource_limits::{
    Admission, EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY, EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW, QUIC_RETRY_REPEAT_INTERVAL, RetryAdmission,
};
use crate::transport::{
    BoxedGrpcIo, Compressed, PreTrustPairingReachability, TrustedPeerConnections,
};
use crate::trust::SharedTrustStore;
use crate::{HostId, audit};

#[derive(Debug, Error)]
pub(crate) enum DispatchError {
    #[error("TLS handshake timed out")]
    Timeout,
    #[error("TLS handshake failed: {0}")]
    Tls(#[from] std::io::Error),
    #[error("dispatcher output channel is closed")]
    ChannelClosed,
    #[error("native link failed: {0}")]
    Link(String),
    /// A link that ran and then ended, which is how every link finishes: the
    /// peer closed it, this side superseded it, or the connection went away.
    /// Held apart from a link that failed so that an ordinary ending is not
    /// reported as a connection this host refused.
    #[error("native link ended: {0}")]
    LinkEnded(String),
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

thread_local! {
    static MTLS_AUDIT_EMITTED: AtomicBool = const { AtomicBool::new(false) };
}

pub(crate) fn mark_mtls_audit_emitted() {
    MTLS_AUDIT_EMITTED.with(|emitted| emitted.store(true, Ordering::Relaxed));
}

fn take_mtls_audit_emitted() -> bool {
    MTLS_AUDIT_EMITTED.with(|emitted| emitted.swap(false, Ordering::Relaxed))
}

/// Whether a handshake ended because the peer went away rather than because it
/// failed to prove who it is.
///
/// Every way a peer can leave mid-handshake reaches this as one of these
/// kinds: TLS reports an unfinished handshake as an unexpected end of file,
/// and QUIC reports a stream the peer stopped or a connection it dropped as a
/// reset or as not connected.
fn peer_went_away(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::NotConnected
    )
}

impl DispatchError {
    /// Whether this is a peer that went away rather than one this host turned
    /// down. A stream opening on a link that is superseded ends this way, and
    /// so does every stream on a connection that drops.
    pub(crate) fn is_peer_leaving(&self) -> bool {
        match self {
            DispatchError::Tls(error) => peer_went_away(error),
            _ => false,
        }
    }
}

/// How a link that ran here finished.
///
/// A link is unavailable when it ends the ordinary way: the peer closed it,
/// this side superseded it with a newer one, or the connection went away.
/// Anything else is a link this host would not serve, which is worth a
/// warning.
fn link_outcome(error: crate::link::LinkError) -> DispatchError {
    match &error {
        crate::link::LinkError::Status(status) if status.code() == tonic::Code::Unavailable => {
            DispatchError::LinkEnded(error.to_string())
        }
        _ => DispatchError::Link(error.to_string()),
    }
}

#[derive(Clone)]
pub(crate) struct TunnelDispatcher {
    acceptor: TlsAcceptor,
    trust_store: SharedTrustStore,
    pair_mode: std::sync::Arc<PairMode>,
    trusted_connections: TrustedPeerConnections,
    trusted_tx: mpsc::Sender<BoxedGrpcIo>,
    pairing_tx: mpsc::Sender<BoxedGrpcIo>,
    handshake_timeout: Duration,
    external_quic_retries: std::sync::Arc<Mutex<RetryAdmission<IpAddr, quinn::ConnectionId>>>,
    external_quic_handshake_slots: std::sync::Arc<Semaphore>,
    link_ctx: Option<LinkCtx>,
}

/// What the dispatcher says, as a QUIC close reason, to a connection that
/// presents no certificate while no pairing window is open. Nothing on this
/// host reads the secret then, so this refusal is a closed window's whole
/// answer to a pairing attempt.
pub(crate) const NO_PAIRING_WINDOW: &[u8] = b"no pairing window is open";

pub(crate) enum DispatchTarget {
    Trusted(HostId),
    Pairing,
    NoPairingWindow,
    Close,
}

impl TunnelDispatcher {
    pub(crate) fn new(
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        pair_mode: std::sync::Arc<PairMode>,
        trusted_connections: TrustedPeerConnections,
        trusted_tx: mpsc::Sender<BoxedGrpcIo>,
        pairing_tx: mpsc::Sender<BoxedGrpcIo>,
        handshake_timeout: Duration,
    ) -> Result<Self, IdentityError> {
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(
            identity.server_tls_config(trust_store.clone())?,
        ));
        Ok(Self {
            acceptor,
            trust_store,
            pair_mode,
            trusted_connections,
            trusted_tx,
            pairing_tx,
            handshake_timeout,
            external_quic_retries: std::sync::Arc::new(Mutex::new(RetryAdmission::new(
                EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
                EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW,
                QUIC_RETRY_REPEAT_INTERVAL,
            ))),
            external_quic_handshake_slots: std::sync::Arc::new(Semaphore::new(
                EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY,
            )),
            link_ctx: None,
        })
    }

    pub(crate) fn with_link_ctx(mut self, link_ctx: LinkCtx) -> Self {
        self.link_ctx = Some(link_ctx);
        self
    }

    pub(crate) fn serve_quic_endpoint(
        &self,
        endpoint: quinn::Endpoint,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            let mut connection_tasks = tokio::task::JoinSet::new();
            loop {
                let incoming = tokio::select! {
                    incoming = endpoint.accept() => incoming,
                    completed = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                        if let Some(Err(error)) = completed
                            && !error.is_cancelled()
                        {
                            tracing::warn!(error = %error, "external QUIC connection task failed");
                        }
                        continue;
                    }
                    _ = wait_for_shutdown(&mut shutdown_rx) => {
                        connection_tasks.detach_all();
                        break;
                    }
                };
                let Some(incoming) = incoming else { break };
                let addr = incoming.remote_address();

                // Stateless address validation and per-source admission both
                // happen before accepting the Incoming, so quinn has not yet
                // allocated connection or TLS handshake state.
                if !incoming.remote_address_validated() {
                    match dispatcher
                        .admit_external_quic_handshake(addr.ip(), incoming.orig_dst_cid())
                        .await
                    {
                        Admission::Retry => {}
                        Admission::Repeat => {
                            incoming.ignore();
                            continue;
                        }
                        Admission::Refused => {
                            tracing::warn!(peer = %addr, "external QUIC handshake rate limit exceeded");
                            incoming.ignore();
                            continue;
                        }
                    }
                    tracing::debug!(peer = %addr, "answering an unvalidated QUIC Initial with a Retry");
                    if let Err(error) = incoming.retry() {
                        tracing::debug!(peer = %addr, error = %error, "QUIC address was already validated");
                        error.into_incoming().ignore();
                    }
                    continue;
                };
                let permit = match dispatcher
                    .external_quic_handshake_slots
                    .clone()
                    .try_acquire_owned()
                {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(peer = %addr, "external QUIC handshake concurrency limit exceeded");
                        incoming.refuse();
                        continue;
                    }
                };
                let dispatcher = dispatcher.clone();
                let connection_shutdown = shutdown_rx.clone();
                connection_tasks.spawn(async move {
                    let _permit = permit;
                    let connection = match tokio::time::timeout(
                        dispatcher.handshake_timeout,
                        incoming,
                    )
                    .await
                    {
                        Ok(Ok(connection)) => {
                            let stats = connection.stats();
                            tracing::debug!(
                                peer = %addr,
                                rtt_ms = stats.path.rtt.as_millis(),
                                sent = stats.path.sent_packets,
                                lost = stats.path.lost_packets,
                                "QUIC connection accepted"
                            );
                            connection
                        }
                        Ok(Err(error)) => {
                            audit::auth_mtls_handshake_failure(&error);
                            tracing::warn!(peer = %addr, error = %error, "QUIC handshake failed");
                            return;
                        }
                        Err(_) => {
                            audit::auth_mtls_handshake_failure("QUIC handshake timed out");
                            tracing::warn!(peer = %addr, "QUIC handshake timed out");
                            return;
                        }
                    };
                    drop(_permit);
                    match dispatcher.dispatch_quic(connection, connection_shutdown).await {
                        Ok(()) => {}
                        Err(DispatchError::LinkEnded(reason)) => {
                            tracing::debug!(peer = %addr, %reason, "link from this peer ended");
                        }
                        Err(error) => {
                            tracing::warn!(peer = %addr, error = %error, "dispatcher rejected QUIC connection");
                        }
                    }
                });
            }
        })
    }

    pub(crate) async fn dispatch_link_stream(
        &self,
        inbound: InboundStream,
    ) -> Result<(), DispatchError> {
        let InboundStream {
            adjacent_peer,
            stream,
            plain,
        } = inbound;
        let directly_paired = self
            .trust_store
            .read()
            .map_err(|_| IdentityError::TrustStorePoisoned)?
            .entry(adjacent_peer)
            .is_some();
        if plain {
            // The link authenticated the peer; the stream is its own. A
            // peer this host no longer trusts has its link closed, so a
            // plain stream from one is a stream from a link on its way out.
            if !directly_paired {
                return Err(DispatchError::Link(format!(
                    "a plain stream from {adjacent_peer}, which is not paired"
                )));
            }
            return self
                .trusted_tx
                .send(
                    BoxedGrpcIo::trusted(Compressed::new(stream), adjacent_peer)
                        .track_trusted_peer(&self.trusted_connections),
                )
                .await
                .map_err(|_| DispatchError::ChannelClosed);
        }
        let pairing_reachability = if directly_paired {
            PreTrustPairingReachability::NoReusableReachability
        } else {
            PreTrustPairingReachability::Cloud
        };
        self.dispatch(stream, pairing_reachability).await
    }

    async fn admit_external_quic_handshake(
        &self,
        source: IpAddr,
        attempt: quinn::ConnectionId,
    ) -> Admission {
        self.external_quic_retries
            .lock()
            .await
            .admit(source, attempt)
    }

    async fn dispatch_quic(
        &self,
        connection: quinn::Connection,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<(), DispatchError> {
        match self.classify_quic(&connection)? {
            DispatchTarget::Trusted(peer) => {
                let ctx = self.link_ctx.clone().ok_or_else(|| {
                    DispatchError::Link("native link context is not configured".into())
                })?;
                run_link(
                    ctx.with_authenticated_peer(peer).with_shutdown(shutdown_rx),
                    std::sync::Arc::new(QuicCarrier::from_accepted(connection)),
                    crate::routing::ConnectRole::Acceptor,
                )
                .await
                .map_err(link_outcome)
            }
            DispatchTarget::Pairing => {
                let (send, recv) =
                    tokio::time::timeout(self.handshake_timeout, connection.accept_bi())
                        .await
                        .map_err(|_| DispatchError::Timeout)?
                        .map_err(|error| DispatchError::Link(error.to_string()))?;
                self.pairing_tx
                    .send(BoxedGrpcIo::pre_trust_pairing(
                        accepted_quic_bidi_stream(send, recv),
                        PreTrustPairingReachability::Direct(connection.remote_address()),
                    ))
                    .await
                    .map_err(|_| DispatchError::ChannelClosed)
            }
            DispatchTarget::NoPairingWindow => {
                connection.close(0_u32.into(), NO_PAIRING_WINDOW);
                Ok(())
            }
            DispatchTarget::Close => {
                connection.close(0_u32.into(), b"connection is not admitted");
                Ok(())
            }
        }
    }

    pub(crate) fn classify_quic(
        &self,
        connection: &quinn::Connection,
    ) -> Result<DispatchTarget, IdentityError> {
        let peer_certificates = connection
            .peer_identity()
            .map(|identity| {
                identity
                    .downcast::<Vec<CertificateDer<'static>>>()
                    .map_err(|_| {
                        IdentityError::TlsConfig(
                            "QUIC peer identity was not a certificate chain".into(),
                        )
                    })
            })
            .transpose()?;
        self.dispatch_target_for_certificate(peer_certificates.as_deref().and_then(|c| c.first()))
    }

    async fn dispatch<IO>(
        &self,
        stream: IO,
        pairing_reachability: PreTrustPairingReachability,
    ) -> Result<(), DispatchError>
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let tls_stream = self.accept_tls(stream).await?;

        match self.dispatch_target(&tls_stream)? {
            // A paired host's channel is compressed inside its handshake, as
            // the opener's is (`ChannelPool::secure_channel`); pairing's is not.
            DispatchTarget::Trusted(peer) => self
                .trusted_tx
                .send(
                    BoxedGrpcIo::trusted(Compressed::new(tls_stream), peer)
                        .track_trusted_peer(&self.trusted_connections),
                )
                .await
                .map_err(|_| DispatchError::ChannelClosed),
            DispatchTarget::Pairing => self
                .pairing_tx
                .send(BoxedGrpcIo::pre_trust_pairing(
                    tls_stream,
                    pairing_reachability,
                ))
                .await
                .map_err(|_| DispatchError::ChannelClosed),
            DispatchTarget::NoPairingWindow | DispatchTarget::Close => Ok(()),
        }
    }

    async fn accept_tls<IO>(
        &self,
        stream: IO,
    ) -> Result<tokio_rustls::server::TlsStream<IO>, DispatchError>
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        match tokio::time::timeout(self.handshake_timeout, self.acceptor.accept(stream)).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(error)) => {
                if peer_went_away(&error) {
                    // Not an authentication failure: the peer stopped talking
                    // before it said who it was. A link superseded while a
                    // stream on it was still handshaking ends exactly here,
                    // and that happens on any ordinary reconnect. Auditing it
                    // as a refused identity would bury the real ones.
                    take_mtls_audit_emitted();
                    tracing::debug!(error = %error, "peer left before its handshake finished");
                } else if !take_mtls_audit_emitted() {
                    audit::auth_mtls_handshake_failure(&error);
                }
                Err(DispatchError::Tls(error))
            }
            Err(_) => {
                audit::auth_mtls_handshake_failure("TLS handshake timed out");
                Err(DispatchError::Timeout)
            }
        }
    }

    fn dispatch_target<IO>(
        &self,
        tls_stream: &tokio_rustls::server::TlsStream<IO>,
    ) -> Result<DispatchTarget, IdentityError> {
        let peer_cert = tls_stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|certs| certs.first());

        self.dispatch_target_for_certificate(peer_cert)
    }

    fn dispatch_target_for_certificate(
        &self,
        peer_cert: Option<&CertificateDer<'_>>,
    ) -> Result<DispatchTarget, IdentityError> {
        if let Some(cert) = peer_cert {
            let trust_store = self
                .trust_store
                .read()
                .map_err(|_| IdentityError::TrustStorePoisoned)?;
            return Ok(
                match identity::host_id_for_certificate(&trust_store, cert)? {
                    Some(host_id) => DispatchTarget::Trusted(host_id),
                    None => {
                        audit::auth_mtls_handshake_failure(
                            "trusted client certificate no longer maps to a trusted host",
                        );
                        DispatchTarget::Close
                    }
                },
            );
        }

        // A window closed by too many wrong secrets still takes the
        // connection, so the attempt hears it is locked out.
        if self.pair_mode.is_active() || self.pair_mode.is_locked_out() {
            Ok(DispatchTarget::Pairing)
        } else {
            audit::auth_mtls_handshake_failure(
                "missing client certificate and pairing mode inactive",
            );
            Ok(DispatchTarget::NoPairingWindow)
        }
    }
}

async fn wait_for_shutdown(shutdown_rx: &mut watch::Receiver<bool>) {
    while !*shutdown_rx.borrow_and_update() {
        if shutdown_rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::Arc;

    use chrono::{DateTime, Utc};
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{
        WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
    };
    use rustls::pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
    };
    use rustls::{
        ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version,
    };
    use tempfile::TempDir;
    use tokio_rustls::TlsConnector;
    use tonic::transport::server::Connected;

    use super::*;
    use crate::identity::load_or_create_device_identity_in;
    use crate::transport::BoxedGrpcAuth;
    use crate::trust::{Reachability, TrustEntry, TrustStore};

    #[derive(Clone)]
    struct TestClientConfig {
        identity: Option<DeviceIdentity>,
    }

    #[derive(Clone)]
    struct NoServerVerification {
        supported_algs: WebPkiSupportedAlgorithms,
    }

    impl fmt::Debug for NoServerVerification {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("NoServerVerification").finish()
        }
    }

    impl ServerCertVerifier for NoServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, TlsError> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            verify_tls12_signature(message, cert, dss, &self.supported_algs)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            verify_tls13_signature(message, cert, dss, &self.supported_algs)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported_algs.supported_schemes()
        }
    }

    fn temp_identity() -> (TempDir, DeviceIdentity) {
        let dir = tempfile::tempdir().unwrap();
        let identity = load_or_create_device_identity_in(dir.path()).unwrap();
        (dir, identity)
    }

    fn trust_store_for(identity: &DeviceIdentity) -> TrustStore {
        let mut trust_store = TrustStore::default();
        trust_store.insert_for_test(
            identity.host_id,
            TrustEntry {
                pubkey: identity.public_key().to_vec(),
                name: "peer".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
                signed_in: None,
                platform: None,
            },
        );
        trust_store
    }

    fn client_config(config: TestClientConfig) -> ClientConfig {
        let verifier = Arc::new(NoServerVerification {
            supported_algs: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        });
        let builder = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier);

        let mut config = match config.identity {
            Some(identity) => builder
                .with_client_auth_cert(
                    vec![CertificateDer::from(identity.certificate_der().unwrap())],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                        identity.private_key_pkcs8().to_vec(),
                    )),
                )
                .unwrap(),
            None => builder.with_no_client_auth(),
        };
        config.alpn_protocols = vec![b"h2".to_vec()];
        config
    }

    async fn run_dispatch(
        dispatcher: TunnelDispatcher,
        client_config: ClientConfig,
    ) -> Result<(), DispatchError> {
        run_dispatch_with_reachability(
            dispatcher,
            client_config,
            PreTrustPairingReachability::Cloud,
        )
        .await
    }

    async fn run_dispatch_with_reachability(
        dispatcher: TunnelDispatcher,
        client_config: ClientConfig,
        reachability: PreTrustPairingReachability,
    ) -> Result<(), DispatchError> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let connector = TlsConnector::from(Arc::new(client_config));
        let client = tokio::spawn(async move {
            connector
                .connect(ServerName::try_from("amux.test").unwrap(), client_io)
                .await
        });
        let server = dispatcher.dispatch(server_io, reachability).await;
        let _ = client.await.unwrap();
        server
    }

    #[tokio::test]
    async fn dispatches_trusted_client_cert_to_trusted_server() {
        let (_server_dir, server_identity) = temp_identity();
        let (_peer_dir, peer_identity) = temp_identity();
        let trust_store = trust_store_for(&peer_identity);
        let pair_mode = Arc::new(PairMode::default());
        let (trusted_tx, mut trusted_rx) = mpsc::channel(1);
        let (pairing_tx, mut pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(trust_store)),
            pair_mode,
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();

        run_dispatch(
            dispatcher,
            client_config(TestClientConfig {
                identity: Some(peer_identity.clone()),
            }),
        )
        .await
        .unwrap();

        let trusted = trusted_rx.try_recv().unwrap();
        assert_eq!(
            trusted.connect_info().auth,
            BoxedGrpcAuth::Trusted {
                peer: peer_identity.host_id
            }
        );
        assert!(pairing_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn dispatches_anonymous_tls_only_when_pair_mode_is_active() {
        let (_server_dir, server_identity) = temp_identity();
        let pair_mode = Arc::new(PairMode::default());
        let (trusted_tx, mut trusted_rx) = mpsc::channel(1);
        let (pairing_tx, mut pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            pair_mode.clone(),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();

        run_dispatch(
            dispatcher.clone(),
            client_config(TestClientConfig { identity: None }),
        )
        .await
        .unwrap();
        assert!(trusted_rx.try_recv().is_err());
        assert!(pairing_rx.try_recv().is_err());

        pair_mode.publish(true, false);
        run_dispatch(
            dispatcher,
            client_config(TestClientConfig { identity: None }),
        )
        .await
        .unwrap();
        assert!(trusted_rx.try_recv().is_err());
        let pairing = tokio::time::timeout(Duration::from_secs(1), pairing_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pairing.connect_info().auth,
            BoxedGrpcAuth::PreTrustPairing {
                reachability: PreTrustPairingReachability::Cloud,
            }
        );
    }

    #[tokio::test]
    async fn dispatches_direct_pairing_without_reusable_reachability() {
        let (_server_dir, server_identity) = temp_identity();
        let pair_mode = Arc::new(PairMode::default());
        let (trusted_tx, mut trusted_rx) = mpsc::channel(1);
        let (pairing_tx, mut pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            pair_mode.clone(),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();
        pair_mode.publish(true, false);

        run_dispatch_with_reachability(
            dispatcher,
            client_config(TestClientConfig { identity: None }),
            PreTrustPairingReachability::NoReusableReachability,
        )
        .await
        .unwrap();

        assert!(trusted_rx.try_recv().is_err());
        let pairing = tokio::time::timeout(Duration::from_secs(1), pairing_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pairing.connect_info().auth,
            BoxedGrpcAuth::PreTrustPairing {
                reachability: PreTrustPairingReachability::NoReusableReachability,
            }
        );
    }

    #[tokio::test]
    async fn a_peer_that_leaves_before_it_says_who_it_is_is_not_an_authentication_failure() {
        // What an ordinary reconnect looks like from here: the phone's second
        // link is refused, and a stream still handshaking on it is cut off.
        let (_server_dir, server_identity) = temp_identity();
        let (trusted_tx, _trusted_rx) = mpsc::channel(1);
        let (pairing_tx, _pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            Arc::new(PairMode::default()),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();

        let (server_io, client_io) = tokio::io::duplex(64);
        drop(client_io);
        let error = dispatcher.accept_tls(server_io).await.unwrap_err();
        let DispatchError::Tls(error) = error else {
            panic!("expected a TLS error, got {error:?}");
        };
        assert!(peer_went_away(&error), "{error}");
    }

    #[tokio::test]
    async fn a_peer_that_speaks_no_tls_is_an_authentication_failure() {
        let (_server_dir, server_identity) = temp_identity();
        let (trusted_tx, _trusted_rx) = mpsc::channel(1);
        let (pairing_tx, _pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            Arc::new(PairMode::default()),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();

        let (server_io, mut client_io) = tokio::io::duplex(64);
        let speaking = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = client_io.write_all(&[0xff; 64]).await;
            // Held open, so what the handshake reports is the nonsense it read
            // rather than the peer hanging up.
            std::future::pending::<()>().await;
        });
        let error = dispatcher.accept_tls(server_io).await.unwrap_err();
        speaking.abort();
        let DispatchError::Tls(error) = error else {
            panic!("expected a TLS error, got {error:?}");
        };
        assert!(!peer_went_away(&error), "{error}");
    }

    #[test]
    fn a_link_that_ended_is_not_a_connection_this_host_refused() {
        // What every reconnect leaves behind: the link the previous process
        // opened is superseded by the new one and finishes here.
        let superseded = link_outcome(crate::link::LinkError::Status(tonic::Status::unavailable(
            "direct link superseded",
        )));
        assert!(
            matches!(superseded, DispatchError::LinkEnded(_)),
            "{superseded}"
        );

        let refused = link_outcome(crate::link::LinkError::Status(
            tonic::Status::permission_denied("peer trust was replaced"),
        ));
        assert!(matches!(refused, DispatchError::Link(_)), "{refused}");
    }

    #[test]
    fn a_quic_stream_the_peer_stopped_counts_as_the_peer_leaving() {
        // The kinds quinn gives these, which is what tells them apart from a
        // refused identity without reading any error text.
        let stopped = std::io::Error::from(quinn::WriteError::Stopped(quinn::VarInt::from_u32(0)));
        let reset = std::io::Error::from(quinn::ReadError::Reset(quinn::VarInt::from_u32(0)));
        let closed = std::io::Error::from(quinn::WriteError::ClosedStream);
        for error in [stopped, reset, closed] {
            assert!(peer_went_away(&error), "{error}");
        }
        assert!(!peer_went_away(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid peer certificate"
        )));
    }

    #[tokio::test]
    async fn external_quic_handshake_limiter_is_per_source_ip() {
        let (_server_dir, server_identity) = temp_identity();
        let (trusted_tx, _trusted_rx) = mpsc::channel(1);
        let (pairing_tx, _pairing_rx) = mpsc::channel(1);
        let dispatcher = TunnelDispatcher::new(
            &server_identity,
            Arc::new(std::sync::RwLock::new(TrustStore::default())),
            Arc::new(PairMode::default()),
            TrustedPeerConnections::default(),
            trusted_tx,
            pairing_tx,
            Duration::from_secs(1),
        )
        .unwrap();
        let source = "127.0.0.1".parse::<IpAddr>().unwrap();
        let other = "127.0.0.2".parse::<IpAddr>().unwrap();

        let attempt = |n: u8| quinn::ConnectionId::new(&[n; 8]);
        for n in 0..EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT as u8 {
            assert_eq!(
                dispatcher
                    .admit_external_quic_handshake(source, attempt(n))
                    .await,
                Admission::Retry
            );
        }
        assert_eq!(
            dispatcher
                .admit_external_quic_handshake(source, attempt(99))
                .await,
            Admission::Refused
        );
        assert_eq!(
            dispatcher
                .admit_external_quic_handshake(other, attempt(99))
                .await,
            Admission::Retry
        );
    }
}
