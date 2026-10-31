//! A host reduced to its links, for measuring the relay: it links to the
//! relay over QUIC on UDP or over the relay's TCP carrier, presenting a
//! connection token, dials
//! the account's other hosts through that link as a daemon does, and opens
//! raw streams on those end-to-end links. It runs no handshake inside a
//! stream and serves no calls, so what a measurement through it sees is
//! the relay's forwarding and the links' own QUIC.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use super::framed::{FramedKind, StreamOnly};
use super::quic::LinkEndpoint;
use super::resumption::SessionTickets;
use super::run::spawn_connector;
use super::socket::LinkSocket;
use super::{ByteStream, LinkCtx, QuicCarrier};
use crate::identity::DeviceIdentity;
use crate::link::run::{LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher};
use crate::routing::{Capabilities, LinkRegistry, LiveLocalHost, local_host};
use crate::trust::{SharedTrustStore, TrustStore};
use crate::{HostId, Tier};

/// The tickets a host's links to its peers may hold; the measurement
/// resumes none of them.
const TICKETS: usize = 16;

pub struct RelayHost {
    identity: DeviceIdentity,
    trust: SharedTrustStore,
    endpoint: LinkEndpoint,
    /// The link to the relay, through which every peer link is forwarded.
    through: daemon_core::LinkId,
    relay: Arc<QuicCarrier>,
    relay_link: JoinHandle<Result<(), tonic::Status>>,
    /// The end-to-end link to each peer, whichever side dialled it.
    peers: watch::Sender<HashMap<HostId, Arc<QuicCarrier>>>,
    inbound: Mutex<mpsc::UnboundedReceiver<ByteStream>>,
    inbound_tx: mpsc::UnboundedSender<ByteStream>,
    /// Answers the links peers dial, and hands every link's streams over.
    links: std::sync::Mutex<JoinSet<()>>,
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
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (identity, trust, server) = Self::identity()?;
        // Every packet goes through the relay: the socket's only path is
        // the byte stream to it.
        let endpoint = LinkEndpoint::new(Arc::new(StreamOnly), identity.host_id, Some(server))
            .map_err(|error| unavailable(&error))?;
        let relay =
            QuicCarrier::connect_framed(&endpoint, io, FramedKind::RelayTcp, quic, server_name)
                .await
                .map_err(|error| unavailable(&error))?;
        Self::link(identity, trust, endpoint, relay, token, expires_at).await
    }

    /// Links to a relay at `relay` over QUIC on a UDP socket of the host's
    /// own, as a deployed host does, and returns once the relay has
    /// accepted the link.
    pub async fn connect_udp(
        relay: SocketAddr,
        quic: quinn::ClientConfig,
        server_name: &str,
        token: String,
        expires_at: SystemTime,
    ) -> Result<Self, tonic::Status> {
        let (identity, trust, server) = Self::identity()?;
        let bind = SocketAddr::new(
            match relay {
                SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
                SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
            },
            0,
        );
        let udp = std::net::UdpSocket::bind(bind).map_err(|error| unavailable(&error))?;
        let substrate = LinkSocket::on_udp(udp).map_err(|error| unavailable(&error))?;
        let endpoint = LinkEndpoint::new(substrate, identity.host_id, Some(server))
            .map_err(|error| unavailable(&error))?;
        let relay = QuicCarrier::connect_relay_with_config(&endpoint, relay, server_name, quic)
            .await
            .map_err(|error| unavailable(&error))?;
        Self::link(identity, trust, endpoint, relay, token, expires_at).await
    }

    /// A fresh identity, the trust store its peers are pinned in, and the
    /// configuration it answers their links with.
    fn identity() -> Result<(DeviceIdentity, SharedTrustStore, quinn::ServerConfig), tonic::Status>
    {
        let identity =
            DeviceIdentity::generated(HostId::new_v4()).map_err(|error| unavailable(&error))?;
        let trust: SharedTrustStore = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let server = identity
            .quic_server_config(trust.clone(), &SessionTickets::in_memory(TICKETS))
            .map_err(|error| unavailable(&error))?;
        Ok((identity, trust, server))
    }

    /// Presents `token` on `relay`, the QUIC connection to the relay, and
    /// returns once the relay has accepted the link.
    async fn link(
        identity: DeviceIdentity,
        trust: SharedTrustStore,
        endpoint: LinkEndpoint,
        relay: QuicCarrier,
        token: String,
        expires_at: SystemTime,
    ) -> Result<Self, tonic::Status> {
        let id = identity.host_id;
        let relay = Arc::new(relay);
        let through = crate::routing::LinkId::new(id).core();
        let via = relay
            .remote_address()
            .ok_or_else(|| tonic::Status::unavailable("the relay's link has no address"))?;
        endpoint
            .socket
            .add_forwarding(through, via, relay.path_kind());

        let host = local_host(
            id,
            "relay-host",
            Capabilities {
                cloud_relay: false,
                kinds: Vec::new(),
            },
            true,
        );
        let ctx = LinkCtx::new_live(LiveLocalHost::new(host), Arc::new(LinkRegistry::new(id)));
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
        let (relay_link, established) = spawn_connector(
            ctx.with_connector_auth(auth, None)
                .with_shutdown(shutdown_rx),
            relay.clone(),
        );
        established
            .await
            .map_err(|_| tonic::Status::unavailable("link ended before the relay answered"))??;

        let (peers, _) = watch::channel(HashMap::new());
        let (inbound_tx, inbound) = mpsc::unbounded_channel();
        let mut links = JoinSet::new();
        links.spawn(accept_links(
            endpoint.endpoint.clone(),
            trust.clone(),
            peers.clone(),
            inbound_tx.clone(),
        ));
        Ok(Self {
            identity,
            trust,
            endpoint,
            through,
            relay,
            relay_link,
            peers,
            inbound: Mutex::new(inbound),
            inbound_tx,
            links: std::sync::Mutex::new(links),
            _shutdown: shutdown,
        })
    }

    pub fn id(&self) -> HostId {
        self.identity.host_id
    }

    /// Pins `peer`'s key, as pairing does, so links between the two
    /// authenticate.
    pub fn trust(&self, peer: &RelayHost) -> Result<(), tonic::Status> {
        self.trust
            .write()
            .map_err(|_| tonic::Status::internal("the trust store is poisoned"))?
            .upsert_paired_peer(
                peer.id(),
                peer.identity.public_key().to_vec(),
                "relay-host".to_owned(),
                None,
            )
            .map_err(|error| tonic::Status::internal(error.to_string()))?;
        Ok(())
    }

    /// Dials `peer` through the relay, and returns once the end-to-end
    /// link is up. Both hosts must trust each other first.
    pub async fn link_to(&self, peer: HostId) -> Result<(), tonic::Status> {
        let carrier = QuicCarrier::connect_through(
            &self.endpoint,
            self.through,
            &self.identity,
            self.trust.clone(),
            peer,
        )
        .await
        .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
        let carrier = Arc::new(carrier);
        self.peers.send_modify(|peers| {
            peers.insert(peer, carrier.clone());
        });
        self.links
            .lock()
            .expect("the links lock is poisoned")
            .spawn(hand_over(carrier, self.inbound_tx.clone()));
        Ok(())
    }

    /// Opens a stream on the end-to-end link to `peer`, waiting for the
    /// link where the peer dialled it and its handshake is still landing
    /// here.
    pub async fn open(&self, peer: HostId) -> std::io::Result<ByteStream> {
        let carrier = self
            .peers
            .subscribe()
            .wait_for(|peers| peers.contains_key(&peer))
            .await
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotConnected))?
            .get(&peer)
            .cloned()
            .expect("waited for");
        carrier
            .open_stream()
            .await
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotConnected, error))
    }

    /// The next stream a peer opened to this host, or None once the host
    /// is shutting down.
    pub async fn accept(&self) -> Option<ByteStream> {
        self.inbound.lock().await.recv().await
    }
}

/// Answers the links peers dial, handing each link's streams to the host.
async fn accept_links(
    endpoint: quinn::Endpoint,
    trust: SharedTrustStore,
    peers: watch::Sender<HashMap<HostId, Arc<QuicCarrier>>>,
    inbound: mpsc::UnboundedSender<ByteStream>,
) {
    let mut links = JoinSet::new();
    while let Some(incoming) = endpoint.accept().await {
        let trust = trust.clone();
        let peers = peers.clone();
        let inbound = inbound.clone();
        links.spawn(async move {
            let Ok(connection) = incoming.await else {
                return;
            };
            let carrier = Arc::new(QuicCarrier::from_accepted(connection));
            let Some(peer) = carrier.pinned_peer(&trust) else {
                return;
            };
            peers.send_modify(|peers| {
                peers.insert(peer, carrier.clone());
            });
            hand_over(carrier, inbound).await;
        });
    }
}

/// Hands every stream the peer opens on `carrier` to the host.
async fn hand_over(carrier: Arc<QuicCarrier>, inbound: mpsc::UnboundedSender<ByteStream>) {
    while let Some(stream) = carrier.accept_stream().await {
        if inbound.send(stream).is_err() {
            return;
        }
    }
}

impl Drop for RelayHost {
    fn drop(&mut self) {
        self.links
            .lock()
            .expect("the links lock is poisoned")
            .abort_all();
        for carrier in self.peers.borrow().values() {
            carrier.close(wire::LinkCloseReason::UserShutdown);
        }
        self.relay.close(wire::LinkCloseReason::UserShutdown);
        self.relay_link.abort();
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

fn unavailable(error: &dyn std::fmt::Display) -> tonic::Status {
    tonic::Status::unavailable(error.to_string())
}
