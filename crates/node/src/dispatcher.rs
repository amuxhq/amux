use std::net::IpAddr;
use std::time::Duration;

use rustls::pki_types::CertificateDer;
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;

use crate::driver::pairing::PairWindow as PairMode;
use crate::identity::{self, IdentityError};
use crate::link::link_log::PathKind;
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
    #[error("the pairing connection opened no stream in time")]
    Timeout,
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
        trust_store: SharedTrustStore,
        pair_mode: std::sync::Arc<PairMode>,
        trusted_connections: TrustedPeerConnections,
        trusted_tx: mpsc::Sender<BoxedGrpcIo>,
        pairing_tx: mpsc::Sender<BoxedGrpcIo>,
        handshake_timeout: Duration,
    ) -> Self {
        Self {
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
        }
    }

    pub(crate) fn with_link_ctx(mut self, link_ctx: LinkCtx) -> Self {
        self.link_ctx = Some(link_ctx);
        self
    }

    pub(crate) fn serve_quic_endpoint(
        &self,
        endpoint: crate::link::LinkEndpoint,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            let mut connection_tasks = tokio::task::JoinSet::new();
            loop {
                let incoming = tokio::select! {
                    incoming = endpoint.endpoint.accept() => incoming,
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
                // Admission counts by the sender's real address, not the
                // stand-in the link socket shows quinn; a host dialled through
                // a forwarder has none of its own here.
                let addr = endpoint.address_of(incoming.remote_address());
                let forwarded = endpoint.arrived_forwarded(incoming.remote_address());
                let path = forwarded.unwrap_or_else(|| PathKind::of_addr(addr));

                // Stateless address validation and per-source admission both
                // happen before accepting the Incoming, so quinn has not yet
                // allocated connection or TLS handshake state. A host dialled
                // through a forwarder needs neither: the forwarder names the
                // sender, and carries only hosts it admitted.
                if forwarded.is_none() && !incoming.remote_address_validated() {
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
                }
                // A token from an earlier connection skips the Retry, not
                // the limit.
                if forwarded.is_none()
                    && !dispatcher
                        .external_quic_retries
                        .lock()
                        .await
                        .admit_validated(addr.ip(), incoming.orig_dst_cid())
                {
                    tracing::warn!(peer = %addr, "external QUIC handshake rate limit exceeded");
                    incoming.ignore();
                    continue;
                }
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
                    match dispatcher
                        .dispatch_quic(connection, addr, path, connection_shutdown)
                        .await
                    {
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

    /// Serves a stream a paired host opened on its link: the link's
    /// handshake authenticated the host, and the stream is its own. A host
    /// this one no longer trusts has its link closed, so a stream from one
    /// is a stream from a link on its way out.
    pub(crate) async fn dispatch_link_stream(
        &self,
        inbound: InboundStream,
    ) -> Result<(), DispatchError> {
        let InboundStream {
            adjacent_peer,
            stream,
        } = inbound;
        let paired = self
            .trust_store
            .read()
            .map_err(|_| IdentityError::TrustStorePoisoned)?
            .entry(adjacent_peer)
            .is_some();
        if !paired {
            return Err(DispatchError::Link(format!(
                "a stream from {adjacent_peer}, which is not paired"
            )));
        }
        // Compressed as the opener's end is (`ChannelPool::channel_over`).
        self.trusted_tx
            .send(
                BoxedGrpcIo::trusted(Compressed::new(stream), adjacent_peer)
                    .track_trusted_peer(&self.trusted_connections),
            )
            .await
            .map_err(|_| DispatchError::ChannelClosed)
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
        addr: std::net::SocketAddr,
        path: PathKind,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<(), DispatchError> {
        match self.classify_quic(&connection)? {
            DispatchTarget::Trusted(peer) => {
                let ctx = self.link_ctx.clone().ok_or_else(|| {
                    DispatchError::Link("native link context is not configured".into())
                })?;
                run_link(
                    ctx.with_authenticated_peer(peer).with_shutdown(shutdown_rx),
                    std::sync::Arc::new(QuicCarrier::from_accepted(connection).with_path(path)),
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
                        // A host dialled through the relay is reached through
                        // it again; one dialled direct, at its address.
                        match path {
                            PathKind::RelayUdp | PathKind::RelayTcp => {
                                PreTrustPairingReachability::Cloud
                            }
                            _ => PreTrustPairingReachability::Direct(addr),
                        },
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
    use super::*;

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
}
