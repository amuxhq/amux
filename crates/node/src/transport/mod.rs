//! Transport helpers for gRPC services.

mod compress;
mod io;
mod memory;
mod pairing;
mod relay_tls;
mod single_io;
mod ssh;
mod tcp;
mod tls;

pub(crate) use compress::Compressed;
pub(crate) use io::{
    BoxedGrpcAuth, BoxedGrpcConnectInfo, BoxedGrpcIo, PreTrustPairingReachability,
    TrustedPeerConnections,
};
pub(crate) use memory::ShutdownIo;
pub(crate) use pairing::{pairing_quic_channel, pairing_quic_channel_through};
pub use relay_tls::{
    create_tls_acceptor, relay_quic_server_config, relay_quic_server_config_from_der,
};
pub(crate) use single_io::channel_from_single_io;
#[allow(unused_imports)]
pub(crate) use ssh::{SshRelayIo, spawn_ssh_pair_recv, spawn_ssh_relay};
#[allow(unused_imports)]
pub(crate) use tcp::tcp_incoming;
use thiserror::Error;
pub use tls::relay_quic_client_config_with_roots;
pub(crate) use tls::{cloud_root_store, relay_quic_client_config_keeping, tls_connect_stream};

pub(crate) type Result<T> = std::result::Result<T, TransportError>;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Transport config error: {0}")]
    Config(String),
    #[error("no pairing window is open")]
    NoPairingWindow,
}
