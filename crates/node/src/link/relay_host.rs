//! A host reduced to its relay link, for measuring the relay: it presents a
//! connection token, opens raw streams through the relay to the account's
//! other hosts and hands over the streams they open to it. It runs no
//! handshake inside a stream and serves no calls, so what a measurement
//! through it sees is the relay's own work.

use std::sync::Arc;
use std::time::SystemTime;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;
use wire::pb;

use super::framed::{FramedKind, StreamEndpoint};
use super::run::spawn_connector_with_auth_establishment_and_shutdown;
use super::{ByteStream, InboundStream, LinkCarrier, LinkCtx, QuicCarrier};
use crate::routing::{
    Capabilities, LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher, LinkRegistry,
    LiveLocalHost, local_host,
};
use crate::{HostId, Tier};

pub struct RelayHost {
    id: HostId,
    carrier: Arc<QuicCarrier>,
    inbound: Mutex<mpsc::Receiver<InboundStream>>,
    link: JoinHandle<Result<(), tonic::Status>>,
    _shutdown: watch::Sender<bool>,
}

impl RelayHost {
    /// Links to a relay over `io`, a byte stream to its TCP carrier, with
    /// the QUIC handshake `quic` makes to `server_name` inside it,
    /// presenting `token`, and returns once the relay has accepted the link.
    pub async fn connect<IO>(
        io: IO,
        quic: quinn::ClientConfig,
        server_name: &str,
        token: String,
        expires_at: SystemTime,
    ) -> Result<Self, tonic::Status>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let id = HostId::new_v4();
        let (inbound_tx, inbound) = mpsc::channel(64);
        let host = local_host(
            id,
            "relay-host",
            Capabilities {
                cloud_relay: false,
                kinds: Vec::new(),
            },
            true,
        );
        let ctx = LinkCtx::new_live(LiveLocalHost::new(host), Arc::new(LinkRegistry::new(id)))
            .with_incoming_streams(inbound_tx);
        let stream = StreamEndpoint::new(io, FramedKind::RelayTcp, None)
            .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
        let carrier = Arc::new(
            QuicCarrier::connect_over_stream(stream, quic, server_name)
                .await
                .map_err(|error| tonic::Status::unavailable(error.to_string()))?,
        );
        let auth = LinkConnectorAuth::with_free_refresh_interval(
            LinkConnectorToken {
                token,
                expires_at,
                fetched_at: SystemTime::now(),
                tier: Tier::Pro,
            },
            Arc::new(NoRefresh),
            None,
        );
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (link, established) = spawn_connector_with_auth_establishment_and_shutdown(
            ctx,
            carrier.clone(),
            auth,
            shutdown_rx,
            None,
        );
        established
            .await
            .map_err(|_| tonic::Status::unavailable("link ended before the relay answered"))??;
        Ok(Self {
            id,
            carrier,
            inbound: Mutex::new(inbound),
            link,
            _shutdown: shutdown,
        })
    }

    pub fn id(&self) -> HostId {
        self.id
    }

    /// Opens a stream on the link to the relay. The relay carries no
    /// streams to other hosts, so nothing reaches `_peer` this way: hosts
    /// reach each other over links of their own, forwarded by the relay.
    pub async fn open(&self, _peer: HostId) -> std::io::Result<ByteStream> {
        self.carrier
            .open_stream()
            .await
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotConnected, error))
    }

    /// The next stream another host opened to this one, or None once the
    /// link has ended.
    pub async fn accept(&self) -> Option<ByteStream> {
        self.inbound
            .lock()
            .await
            .recv()
            .await
            .map(|inbound| inbound.stream)
    }
}

impl Drop for RelayHost {
    fn drop(&mut self) {
        self.carrier.close(pb::LinkCloseReason::UserShutdown);
        self.link.abort();
    }
}

/// A measurement's credential outlives it, so it is never refreshed.
struct NoRefresh;

#[tonic::async_trait]
impl LinkConnectorTokenRefresher for NoRefresh {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
        Err(tonic::Status::unavailable("a relay host does not refresh"))
    }
}
