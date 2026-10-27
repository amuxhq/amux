//! The relay's own TLS: the WebPKI certificate its TCP listener and its
//! QUIC endpoint present to hosts.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::version;
use tokio_rustls::TlsAcceptor;

use crate::link::resumption::SessionTickets;
use crate::transport::{Result, TransportError};

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
/// certificate and key used by its TCP/TLS listener, issuing session
/// tickets from `tickets`.
pub fn relay_quic_server_config(
    cert_pem: &[u8],
    key_pem: &[u8],
    tickets: &Arc<SessionTickets>,
) -> Result<quinn::ServerConfig> {
    let (certs, key) = parse_server_identity(cert_pem, key_pem)?;
    relay_quic_server_config_from_der(certs, key, tickets)
}

pub fn relay_quic_server_config_from_der(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    tickets: &Arc<SessionTickets>,
) -> Result<quinn::ServerConfig> {
    let tls = rustls::ServerConfig::builder_with_protocol_versions(&[&version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| TransportError::Config(format!("TLS config error: {error}")))?;
    crate::identity::quic_server_config_issuing(tls, tickets, Default::default())
        .map_err(TransportError::Config)
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
