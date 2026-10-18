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

use super::run::spawn_connector_with_auth_establishment_and_shutdown;
use super::{ByteStream, CarrierKind, InboundStream, LinkCarrier, LinkCtx, MuxCarrier, MuxRole};
use crate::routing::{
    Capabilities, LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher, LinkRegistry,
    LiveLocalHost, RoutingCore, local_host,
};
use crate::{HostId, Tier};

pub struct RelayHost {
    id: HostId,
    carrier: Arc<MuxCarrier>,
    inbound: Mutex<mpsc::Receiver<InboundStream>>,
    link: JoinHandle<Result<(), tonic::Status>>,
    _shutdown: watch::Sender<bool>,
}

impl RelayHost {
    /// Links to a relay over `io`, a byte stream to its TCP carrier,
    /// presenting `token`, and returns once the relay has accepted the link.
    pub async fn connect<IO>(
        io: IO,
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
        let ctx = LinkCtx::new_live(
            LiveLocalHost::new(host),
            Arc::new(RoutingCore::new()),
            Arc::new(LinkRegistry::new(id)),
        )
        .with_incoming_streams(inbound_tx);
        let carrier = Arc::new(MuxCarrier::new(
            io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));
        let auth = LinkConnectorAuth::with_free_refresh_interval(
            LinkConnectorToken {
                token,
                expires_at,
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

    /// Opens a stream through the relay to `peer`. Its bytes may follow at
    /// once; a refusal surfaces as the stream's first failed read.
    pub async fn open(&self, peer: HostId) -> std::io::Result<ByteStream> {
        self.carrier
            .open_stream(pb::StreamPreface {
                dst: peer.as_bytes().to_vec(),
                plain: false,
            })
            .await
            .map_err(super::OpenError::into_io)
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
