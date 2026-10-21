//! TLS helpers for server-to-server and cloud connections.
//!
//! [`tls_channel`] establishes an outbound TLS channel (used by cloud client).
//! [`create_tls_acceptor`] builds a `TlsAcceptor` from PEM-encoded cert/key
//! (used by cloud server mode).

use std::fmt;
#[cfg(any(debug_assertions, test))]
use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tonic::transport::{Channel, Endpoint};

use super::{
    channel_from_single_io, configure_relay_tcp_keepalive, configure_tonic_endpoint_keepalive,
};
use crate::transport::{Result, TransportError};

pub(crate) async fn tls_connect_stream(
    host: &str,
    port: u16,
) -> Result<ClientTlsStream<TcpStream>> {
    let root_store = cloud_root_store()?;

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));

    let addr = format!("{}:{}", host, port);
    let stream = TcpStream::connect(&addr).await?;
    stream.set_nodelay(true)?;
    configure_relay_tcp_keepalive(&stream);

    let domain = ServerName::try_from(host.to_string())
        .map_err(|_| TransportError::Config(format!("Invalid DNS name: {}", host)))?;
    let tls_stream = connector.connect(domain, stream).await?;

    Ok(tls_stream)
}

fn cloud_root_store() -> Result<rustls::RootCertStore> {
    let mut root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    add_debug_cloud_root(&mut root_store)?;
    Ok(root_store)
}

#[cfg(any(debug_assertions, test))]
fn add_debug_cloud_root(root_store: &mut rustls::RootCertStore) -> Result<()> {
    let Ok(path) = std::env::var("AMUX_CLOUD_TLS_CA") else {
        return Ok(());
    };
    let pem = std::fs::read(&path).map_err(|error| {
        TransportError::Config(format!(
            "failed to read AMUX_CLOUD_TLS_CA from {path}: {error}"
        ))
    })?;
    let mut added = 0;
    for cert in rustls_pemfile::certs(&mut BufReader::new(pem.as_slice())) {
        let cert = cert.map_err(|error| {
            TransportError::Config(format!("failed to parse AMUX_CLOUD_TLS_CA {path}: {error}"))
        })?;
        root_store.add(cert).map_err(|error| {
            TransportError::Config(format!(
                "invalid AMUX_CLOUD_TLS_CA certificate {path}: {error}"
            ))
        })?;
        added += 1;
    }
    if added == 0 {
        return Err(TransportError::Config(format!(
            "AMUX_CLOUD_TLS_CA {path} contains no certificates"
        )));
    }
    Ok(())
}

#[cfg(not(any(debug_assertions, test)))]
fn add_debug_cloud_root(_root_store: &mut rustls::RootCertStore) -> Result<()> {
    Ok(())
}

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
    let connection = endpoint
        .connect_with(pairing_quic_client_config()?, addr, "amux-pairing.local")
        .map_err(|error| TransportError::Config(error.to_string()))?
        .await
        .map_err(pairing_dial_error)?;
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

pub(crate) async fn pairing_channel_from_io<IO>(io: IO) -> Result<Channel>
where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let connector = TlsConnector::from(Arc::new(pairing_client_config()));
    let server_name =
        ServerName::try_from("amux-pairing.local").expect("static pairing server name is valid");
    let tls = connector.connect(server_name, io).await?;
    Ok(channel_from_single_io(
        configure_tonic_endpoint_keepalive(Endpoint::from_static("https://pairing")),
        "pairing TLS transport",
        tls,
    ))
}

fn pairing_client_config() -> ClientConfig {
    let verifier = Arc::new(NoServerVerification {
        supported_algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
    });
    let mut config = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    config
}

pub fn pairing_quic_client_config() -> Result<quinn::ClientConfig> {
    let mut tls = pairing_client_config();
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

/// Create a TLS acceptor for cloud server mode.
/// Requires TLS certificate and private key files.
pub fn create_tls_acceptor(cert_pem: &[u8], key_pem: &[u8]) -> Result<TlsAcceptor> {
    let (certs, key) = parse_server_identity(cert_pem, key_pem)?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TransportError::Config(format!("TLS config error: {}", e)))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Builds the relay's QUIC server configuration from the same WebPKI
/// certificate and key used by its TCP/TLS listener.
pub fn relay_quic_server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<quinn::ServerConfig> {
    let (certs, key) = parse_server_identity(cert_pem, key_pem)?;
    relay_quic_server_config_from_der(certs, key)
}

pub fn relay_quic_server_config_from_der(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::ServerConfig> {
    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| TransportError::Config(format!("TLS config error: {error}")))?;
    tls.alpn_protocols = vec![crate::identity::QUIC_ALPN.to_vec()];
    tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    tls.send_tls13_tickets = 0;
    tls.max_early_data_size = 0;

    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(crate::identity::quic_transport_config());
    config.migration(true);
    crate::link::validation::issue_tokens(&mut config);
    Ok(config)
}

/// Builds a WebPKI client configuration for the cloud relay's QUIC front.
pub(crate) fn relay_quic_client_config() -> Result<quinn::ClientConfig> {
    relay_quic_client_config_with_roots(cloud_root_store()?)
}

pub fn relay_quic_client_config_with_roots(
    roots: rustls::RootCertStore,
) -> Result<quinn::ClientConfig> {
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
        .with_root_certificates(roots)
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

fn parse_server_identity(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    use std::io::BufReader;

    use rustls_pemfile::{certs, private_key};

    let certs = certs(&mut BufReader::new(cert_pem))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| TransportError::Config(format!("Failed to parse certificate: {error}")))?;
    if certs.is_empty() {
        return Err(TransportError::Config(
            "No certificates found in PEM".to_string(),
        ));
    }
    let key = private_key(&mut BufReader::new(key_pem))
        .map_err(|error| TransportError::Config(format!("Failed to parse private key: {error}")))?
        .ok_or_else(|| TransportError::Config("No private key found in PEM".to_string()))?;
    Ok((certs, key))
}
