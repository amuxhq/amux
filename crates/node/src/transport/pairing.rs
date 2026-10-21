//! The pairing service's QUIC connection: a client with no certificate of
//! its own, answered only while the host's pairing window is open, dialled
//! at an address or through the relay.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version};
use tonic::transport::{Channel, Endpoint};

use super::{channel_from_single_io, configure_tonic_endpoint_keepalive};
use crate::transport::{Result, TransportError};

/// A channel to a host's pairing service over a QUIC connection of its own.
pub(crate) struct PairingQuic {
    pub(crate) channel: Channel,
    connection: quinn::Connection,
}

impl PairingQuic {
    /// Whether the host closed this connection for having no pairing window
    /// open. Its dispatcher decides that only once the handshake completes,
    /// so the refusal can land after the dial succeeded and fail the first
    /// call on the channel instead: a failed call asks this.
    pub(crate) fn refused(&self) -> bool {
        self.connection
            .close_reason()
            .is_some_and(|error| no_pairing_window(&error))
    }
}

fn no_pairing_window(error: &quinn::ConnectionError) -> bool {
    matches!(
        error,
        quinn::ConnectionError::ApplicationClosed(close)
            if close.reason.as_ref() == crate::dispatcher::NO_PAIRING_WINDOW
    )
}

fn pairing_dial_error(error: quinn::ConnectionError) -> TransportError {
    if no_pairing_window(&error) {
        TransportError::NoPairingWindow
    } else {
        TransportError::Config(error.to_string())
    }
}

pub(crate) async fn pairing_quic_channel(
    endpoint: &crate::link::LinkEndpoint,
    addr: SocketAddr,
) -> Result<PairingQuic> {
    let connecting = endpoint
        .connect_with(pairing_quic_client_config()?, addr, PAIRING_SERVER_NAME)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    pairing_quic(connecting).await
}

/// A pairing connection to `peer` through the forwarder at the far end of
/// `through`: the relay, which carries it only for a paid account.
pub(crate) async fn pairing_quic_channel_through(
    endpoint: &crate::link::LinkEndpoint,
    peer: crate::HostId,
    through: crate::link::socket::LinkId,
) -> Result<PairingQuic> {
    let connecting = endpoint
        .connect_through(
            pairing_quic_client_config()?,
            peer,
            through,
            PAIRING_SERVER_NAME,
        )
        .map_err(|error| TransportError::Config(error.to_string()))?;
    pairing_quic(connecting).await
}

const PAIRING_SERVER_NAME: &str = "amux-pairing.local";

async fn pairing_quic(connecting: quinn::Connecting) -> Result<PairingQuic> {
    let connection = connecting.await.map_err(pairing_dial_error)?;
    let (send, recv) = connection.open_bi().await.map_err(pairing_dial_error)?;
    Ok(PairingQuic {
        channel: channel_from_single_io(
            configure_tonic_endpoint_keepalive(Endpoint::from_static("https://pairing")),
            "pairing QUIC stream",
            crate::link::accepted_quic_bidi_stream(send, recv),
        ),
        connection,
    })
}

/// A client presenting no certificate and checking none: the pairing
/// exchange proves both ends to each other with the secret the person
/// carries between them.
pub fn pairing_quic_client_config() -> Result<quinn::ClientConfig> {
    let verifier = Arc::new(NoServerVerification {
        supported_algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
    });
    let mut tls = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![crate::identity::QUIC_ALPN.to_vec()];
    tls.resumption = rustls::client::Resumption::disabled();
    tls.enable_early_data = false;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(crate::identity::quic_transport_config());
    Ok(config)
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
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}
