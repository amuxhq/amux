use std::time::Duration;

use rustls::pki_types::CertificateDer;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::HostId;
use crate::driver::pairing::PairWindow as PairMode;
use crate::identity::{self, IdentityError};
use crate::link::link_log::PathKind;
use crate::link::{InboundStream, LinkCtx, QuicCarrier, accepted_quic_bidi_stream, run_link};
use crate::resource_limits::{Admitted, HandshakeAdmission};
use crate::transport::{BoxedGrpcIo, Compressed, PreTrustPairingReachability, Replacing};
use crate::trust::SharedTrustStore;

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
    replacing: Replacing,
    trusted_tx: mpsc::Sender<BoxedGrpcIo>,
    pairing_tx: mpsc::Sender<BoxedGrpcIo>,
    handshake_timeout: Duration,
    admission: std::sync::Arc<HandshakeAdmission>,
    link_ctx: Option<LinkCtx>,
}

/// What the dispatcher says, as a QUIC close reason, to a connection that
/// presents no certificate while no pairing window is open. Nothing on this
/// host reads the secret then, so this refusal is a closed window's whole
/// answer to a pairing attempt.
pub(crate) const NO_PAIRING_WINDOW: &[u8] = b"no pairing window is open";

/// A connection as far as the dispatcher takes it before serving it.
enum Arrival {
    /// Resuming a session whose ticket names this key, its handshake
    /// still finishing until the watch turns true.
    Resumed(quinn::Connection, Vec<u8>, watch::Receiver<bool>),
    /// Through a full handshake.
    Full(quinn::Connection),
}

/// The key of the certificate a resuming connection's ticket carries, read
/// before its handshake has finished; none for a full handshake, whose
/// certificate is proved only at its end.
fn resumed_key(connection: &quinn::Connection) -> Option<Vec<u8>> {
    let certificates = connection
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    identity::ed25519_public_key_from_certificate(certificates.first()?)
        .ok()
        .map(Vec::from)
}

/// Logs how a connection this host served ended.
fn report_outcome(addr: std::net::SocketAddr, error: DispatchError) {
    match error {
        DispatchError::LinkEnded(reason) => {
            tracing::debug!(peer = %addr, %reason, "link from this peer ended");
        }
        error => {
            tracing::warn!(peer = %addr, error = %error, "dispatcher rejected QUIC connection");
        }
    }
}

pub(crate) enum DispatchTarget {
    Trusted(HostId),
    Pairing,
    NoPairingWindow,
    /// A certificate this host does not pin. The verifier refuses one in
    /// the handshake; this is one whose pin went as the handshake ended.
    Revoked,
}

impl TunnelDispatcher {
    pub(crate) fn new(
        trust_store: SharedTrustStore,
        pair_mode: std::sync::Arc<PairMode>,
        replacing: Replacing,
        trusted_tx: mpsc::Sender<BoxedGrpcIo>,
        pairing_tx: mpsc::Sender<BoxedGrpcIo>,
        handshake_timeout: Duration,
    ) -> Self {
        Self {
            trust_store,
            pair_mode,
            replacing,
            trusted_tx,
            pairing_tx,
            handshake_timeout,
            admission: std::sync::Arc::default(),
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
                let path = forwarded.map_or_else(|| PathKind::of_addr(addr), |(_, kind)| kind);
                let through = forwarded.map(|(through, _)| through);

                // Admission happens before accepting the Incoming, so quinn
                // has not yet allocated connection or TLS handshake state. A
                // host dialled through a forwarder needs no Retry and counts
                // against no source's limit: the forwarder names the sender,
                // and carries only hosts it admitted.
                let Some(Admitted {
                    incoming,
                    permit,
                    spent,
                }) = dispatcher
                    .admission
                    .admit(incoming, addr, forwarded.is_some())
                else {
                    continue;
                };
                let dispatcher = dispatcher.clone();
                let connection_shutdown = shutdown_rx.clone();
                let socket = endpoint.socket.clone();
                connection_tasks.spawn(async move {
                    let connecting = match incoming.accept() {
                        Ok(connecting) => connecting,
                        Err(error) => {
                            tracing::warn!(peer = %addr, error = %error, "QUIC handshake failed");
                            return;
                        }
                    };
                    let Some(arrival) = dispatcher.arrive(connecting, addr, permit).await else {
                        return;
                    };
                    if let Err(error) = dispatcher
                        .dispatch_quic(
                            arrival,
                            addr,
                            (path, through),
                            spent,
                            &socket,
                            connection_shutdown,
                        )
                        .await
                    {
                        report_outcome(addr, error);
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
        if self.replacing.contains(adjacent_peer) {
            return Err(DispatchError::Link(format!(
                "a stream from {adjacent_peer}, whose key is being replaced"
            )));
        }
        // Compressed as the opener's end is (`ChannelPool::channel_over`).
        self.trusted_tx
            .send(BoxedGrpcIo::trusted(Compressed::new(stream), adjacent_peer))
            .await
            .map_err(|_| DispatchError::ChannelClosed)
    }

    /// Takes `connecting` as far as this host must before reading from it.
    /// A connection resuming a session is usable at once, the identity its
    /// ticket names still to be decided, and what it sent early is held
    /// until then; `permit` stays held until its handshake finishes. Any
    /// other is taken through its whole handshake, which proves the
    /// certificate it presents.
    async fn arrive(
        &self,
        connecting: quinn::Connecting,
        addr: std::net::SocketAddr,
        permit: impl Send + 'static,
    ) -> Option<Arrival> {
        let (connection, handshaken) = match connecting.into_0rtt() {
            Ok(usable) => usable,
            Err(connecting) => {
                let connection = self.finish_handshake(connecting, addr).await;
                drop(permit);
                return connection.map(Arrival::Full);
            }
        };
        // rustls restores the client's certificate from the ticket as it
        // reads the ClientHello; a full handshake proves one only at its end.
        if let Some(key) = resumed_key(&connection) {
            let timeout = self.handshake_timeout;
            let handshaking = connection.clone();
            let (over, handshake) = watch::channel(false);
            tokio::spawn(async move {
                let _permit = permit;
                if tokio::time::timeout(timeout, handshaken).await.is_err() {
                    tracing::warn!(peer = %addr, "resumed QUIC handshake timed out");
                    handshaking.close(0_u32.into(), b"handshake timed out");
                }
                over.send_replace(true);
            });
            return Some(Arrival::Resumed(connection, key, handshake));
        }
        let finished = self.finish_handshake(
            async {
                handshaken.await;
                match connection.close_reason() {
                    None => Ok(connection),
                    Some(error) => Err(error),
                }
            },
            addr,
        );
        let connection = finished.await;
        drop(permit);
        connection.map(Arrival::Full)
    }

    async fn finish_handshake(
        &self,
        handshake: impl std::future::IntoFuture<
            Output = Result<quinn::Connection, quinn::ConnectionError>,
        >,
        addr: std::net::SocketAddr,
    ) -> Option<quinn::Connection> {
        match tokio::time::timeout(self.handshake_timeout, handshake).await {
            Ok(Ok(connection)) => {
                let stats = connection.stats();
                tracing::debug!(
                    peer = %addr,
                    rtt_ms = stats.path.rtt.as_millis(),
                    sent = stats.path.sent_packets,
                    lost = stats.path.lost_packets,
                    "QUIC connection accepted"
                );
                Some(connection)
            }
            Ok(Err(error)) => {
                tracing::warn!(peer = %addr, error = %error, "QUIC handshake failed");
                None
            }
            Err(_) => {
                tracing::warn!(peer = %addr, "QUIC handshake timed out");
                None
            }
        }
    }

    async fn dispatch_quic(
        &self,
        arrival: Arrival,
        addr: std::net::SocketAddr,
        (path, through): (PathKind, Option<daemon_core::LinkId>),
        spent: Option<crate::resource_limits::Spent>,
        socket: &std::sync::Arc<crate::link::socket::LinkSocket>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<(), DispatchError> {
        let serve = |ctx: LinkCtx, connection| {
            run_link(
                ctx.with_shutdown(shutdown_rx).refunding(spent),
                std::sync::Arc::new(
                    QuicCarrier::from_accepted(connection)
                        .with_path(path)
                        .carried_through(through)
                        .following(socket.clone()),
                ),
                crate::routing::ConnectRole::Acceptor,
            )
        };
        let link_ctx = || {
            self.link_ctx
                .clone()
                .ok_or_else(|| DispatchError::Link("native link context is not configured".into()))
        };
        let connection = match arrival {
            // The core decides the identity the ticket names, with the
            // host the Hello names, before anything on the link is served.
            Arrival::Resumed(connection, key, handshake) => {
                return serve(link_ctx()?.with_resumed(key, handshake), connection)
                    .await
                    .map_err(link_outcome);
            }
            Arrival::Full(connection) => connection,
        };
        match self.classify_quic(&connection)? {
            DispatchTarget::Trusted(peer) => {
                serve(link_ctx()?.with_authenticated_peer(peer), connection)
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
                        // A host dialled through the relay or a paired host is
                        // reached through it again; one dialled direct, at its
                        // address.
                        match path {
                            PathKind::RelayUdp | PathKind::RelayTcp => {
                                PreTrustPairingReachability::Cloud
                            }
                            PathKind::ViaDaemon => PreTrustPairingReachability::Forwarded,
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
            DispatchTarget::Revoked => {
                connection.close(
                    (wire::pb::LinkCloseReason::UserRevoked as u32).into(),
                    b"this host no longer trusts the connecting one",
                );
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
                        tracing::warn!(
                            "a trusted client certificate no longer maps to a trusted host"
                        );
                        DispatchTarget::Revoked
                    }
                },
            );
        }

        // A window closed by too many wrong secrets still takes the
        // connection, so the attempt hears it is locked out.
        if self.pair_mode.is_active() || self.pair_mode.is_locked_out() {
            Ok(DispatchTarget::Pairing)
        } else {
            tracing::warn!(
                "a connection with no client certificate while no pairing window is open"
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
