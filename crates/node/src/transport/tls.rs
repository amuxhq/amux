//! The WebPKI client a host reaches the relay with: TLS on TCP where UDP is
//! blocked, and the QUIC connection's configuration.

#[cfg(any(debug_assertions, test))]
use std::io::BufReader;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::version;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream as ClientTlsStream;

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

    let domain = ServerName::try_from(host.to_string())
        .map_err(|_| TransportError::Config(format!("Invalid DNS name: {}", host)))?;
    let tls_stream = connector.connect(domain, stream).await?;

    Ok(tls_stream)
}

pub(crate) fn cloud_root_store() -> Result<rustls::RootCertStore> {
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

pub fn relay_quic_client_config_with_roots(
    roots: rustls::RootCertStore,
) -> Result<quinn::ClientConfig> {
    relay_quic_client_config_keeping(roots, None)
}

/// The configuration to dial the relay with, trusting `roots`, keeping
/// the tickets the relay named `server` gives it in `tickets` (loaded into
/// it now) when given, in memory otherwise.
pub(crate) fn relay_quic_client_config_keeping(
    roots: rustls::RootCertStore,
    tickets: Option<(
        Arc<crate::link::client_tickets::ClientTickets>,
        &ServerName<'static>,
    )>,
) -> Result<quinn::ClientConfig> {
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![crate::identity::QUIC_ALPN.to_vec()];
    // A relay link resuming with its ticket writes its Hello, the relay
    // token in it, as early data.
    tls.enable_early_data = true;
    tls.resumption = match &tickets {
        Some((tickets, _)) => rustls::client::Resumption::store(tickets.clone()),
        None => crate::link::resumption::client_resumption(),
    };
    if let Some((tickets, server)) = tickets {
        tickets.load(&tls, server);
    }
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|error| TransportError::Config(error.to_string()))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(crate::identity::quic_transport_config(Default::default()));
    Ok(config)
}
