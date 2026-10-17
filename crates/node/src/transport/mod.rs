//! Transport helpers for gRPC services.

mod compress;
mod io;
mod memory;
mod single_io;
mod ssh;
mod tcp;
mod tls;

use std::time::Duration;

pub(crate) use compress::Compressed;
pub(crate) use io::{
    BoxedGrpcAuth, BoxedGrpcConnectInfo, BoxedGrpcInner, BoxedGrpcIo, PreTrustPairingReachability,
    TrustedPeerConnections,
};
pub(crate) use memory::ShutdownIo;
#[cfg(test)]
pub(crate) use memory::{in_process_channel, in_process_transport_pair};
pub(crate) use single_io::channel_from_single_io;
#[allow(unused_imports)]
pub(crate) use ssh::{SshRelayIo, spawn_ssh_pair_recv, spawn_ssh_relay};
pub(crate) use tcp::configure_relay_tcp_keepalive;
#[allow(unused_imports)]
pub(crate) use tcp::tcp_incoming;
use thiserror::Error;
pub use tls::{
    create_tls_acceptor, relay_quic_client_config_with_roots, relay_quic_server_config,
    relay_quic_server_config_from_der,
};
pub(crate) use tls::{
    pairing_channel_from_io, pairing_quic_channel, relay_quic_client_config, tls_connect_stream,
};
use tonic::transport::{Endpoint, Server};

pub(crate) type Result<T> = std::result::Result<T, TransportError>;

const GRPC_HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const GRPC_HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Transport config error: {0}")]
    Config(String),
    #[error("no pairing window is open")]
    NoPairingWindow,
}

pub(crate) fn configure_tonic_endpoint_keepalive(endpoint: Endpoint) -> Endpoint {
    endpoint
        .http2_keep_alive_interval(GRPC_HTTP2_KEEPALIVE_INTERVAL)
        .keep_alive_timeout(GRPC_HTTP2_KEEPALIVE_TIMEOUT)
        .keep_alive_while_idle(true)
}

pub(crate) fn tonic_server_builder() -> Server {
    Server::builder()
        .http2_keepalive_interval(Some(GRPC_HTTP2_KEEPALIVE_INTERVAL))
        .http2_keepalive_timeout(Some(GRPC_HTTP2_KEEPALIVE_TIMEOUT))
}
