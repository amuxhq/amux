//! Common tonic IO wrapper for transports that do not need connect metadata.

use std::collections::{HashMap, HashSet};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use futures_util::task::AtomicWaker;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::transport::server::Connected;

use crate::HostId;

pub(crate) trait BoxedGrpcInner: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T> BoxedGrpcInner for T where T: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoxedGrpcAuth {
    /// A paired host, authenticated by the TLS handshake inside the stream
    /// or by the direct link that carries it.
    Trusted { peer: HostId },
    PreTrustPairing {
        reachability: PreTrustPairingReachability,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreTrustPairingReachability {
    Cloud,
    /// Dialled directly from this address. A host dials from the same QUIC
    /// endpoint it listens on, so the address it was seen at is where it
    /// can be dialled back.
    Direct(std::net::SocketAddr),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoxedGrpcConnectInfo {
    pub(crate) auth: BoxedGrpcAuth,
}

pub(crate) struct BoxedGrpcIo {
    inner: Box<dyn BoxedGrpcInner>,
    connect_info: BoxedGrpcConnectInfo,
    trusted_connection: Option<TrustedPeerConnection>,
}

impl BoxedGrpcIo {
    pub(crate) fn trusted<T>(inner: T, peer: HostId) -> Self
    where
        T: BoxedGrpcInner,
    {
        Self::new(inner, BoxedGrpcAuth::Trusted { peer })
    }

    pub(crate) fn pre_trust_pairing<T>(inner: T, reachability: PreTrustPairingReachability) -> Self
    where
        T: BoxedGrpcInner,
    {
        Self::new(inner, BoxedGrpcAuth::PreTrustPairing { reachability })
    }

    fn new<T>(inner: T, auth: BoxedGrpcAuth) -> Self
    where
        T: BoxedGrpcInner,
    {
        Self {
            inner: Box::new(inner),
            connect_info: BoxedGrpcConnectInfo { auth },
            trusted_connection: None,
        }
    }

    pub(crate) fn track_trusted_peer(mut self, registry: &TrustedPeerConnections) -> Self {
        if self.trusted_connection.is_none()
            && let BoxedGrpcAuth::Trusted { peer } = self.connect_info.auth
        {
            self.trusted_connection = Some(registry.register(peer));
        }
        self
    }

    fn trusted_peer_revoked(&self, cx: &mut Context<'_>) -> bool {
        self.trusted_connection
            .as_ref()
            .is_some_and(|connection| connection.poll_revoked(cx))
    }
}

impl AsyncRead for BoxedGrpcIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.trusted_peer_revoked(cx) {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedGrpcIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.trusted_peer_revoked(cx) {
            return Poll::Ready(Err(trust_revoked_error()));
        }
        Pin::new(&mut *self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.trusted_peer_revoked(cx) {
            return Poll::Ready(Err(trust_revoked_error()));
        }
        Pin::new(&mut *self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.inner).poll_shutdown(cx)
    }
}

impl Connected for BoxedGrpcIo {
    type ConnectInfo = BoxedGrpcConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.connect_info.clone()
    }
}

fn trust_revoked_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "trusted peer authority was revoked",
    )
}

#[derive(Clone)]
pub(crate) struct TrustedPeerConnections {
    inner: Arc<TrustedPeerConnectionsInner>,
}

struct TrustedPeerConnectionsInner {
    state: std::sync::Mutex<TrustedPeerConnectionsState>,
}

#[derive(Default)]
struct TrustedPeerConnectionsState {
    by_peer: HashMap<HostId, PeerTrustedConnections>,
    replacing: HashSet<HostId>,
}

#[derive(Default)]
struct PeerTrustedConnections {
    next_id: u64,
    active: HashMap<u64, Arc<ActiveTrustedConnection>>,
}

struct ActiveTrustedConnection {
    revoked: AtomicBool,
    waker: AtomicWaker,
}

pub(crate) struct TrustedPeerConnection {
    registry: TrustedPeerConnections,
    peer: HostId,
    id: u64,
    active: Arc<ActiveTrustedConnection>,
}

impl Default for TrustedPeerConnections {
    fn default() -> Self {
        Self {
            inner: Arc::new(TrustedPeerConnectionsInner {
                state: std::sync::Mutex::new(TrustedPeerConnectionsState::default()),
            }),
        }
    }
}

impl TrustedPeerConnections {
    fn register(&self, peer: HostId) -> TrustedPeerConnection {
        let active = Arc::new(ActiveTrustedConnection {
            revoked: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        });
        let id = {
            let mut state = self.inner.state.lock().expect("trusted registry poisoned");
            let revoked = state.replacing.contains(&peer);
            active.revoked.store(revoked, Ordering::Release);
            let peer_state = state.by_peer.entry(peer).or_default();
            let id = peer_state.next_id;
            peer_state.next_id = peer_state.next_id.wrapping_add(1);
            peer_state.active.insert(id, active.clone());
            id
        };
        TrustedPeerConnection {
            registry: self.clone(),
            peer,
            id,
            active,
        }
    }

    pub(crate) async fn close_host(&self, peer: HostId) -> usize {
        let active = {
            let mut state = self.inner.state.lock().expect("trusted registry poisoned");
            state.replacing.insert(peer);
            state
                .by_peer
                .get(&peer)
                .map(|peer_state| peer_state.active.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for connection in &active {
            connection.revoked.store(true, Ordering::Release);
            connection.waker.wake();
        }
        active.len()
    }

    pub(crate) fn finish_host_replacement(&self, peer: HostId) {
        self.inner
            .state
            .lock()
            .expect("trusted registry poisoned")
            .replacing
            .remove(&peer);
    }

    fn remove(&self, peer: HostId, id: u64) {
        {
            let mut state = self.inner.state.lock().expect("trusted registry poisoned");
            let Some(peer_state) = state.by_peer.get_mut(&peer) else {
                return;
            };
            peer_state.active.remove(&id);
            if peer_state.active.is_empty() {
                state.by_peer.remove(&peer);
            }
        }
    }
}

impl TrustedPeerConnection {
    fn poll_revoked(&self, cx: &mut Context<'_>) -> bool {
        if self.active.revoked.load(Ordering::Acquire) {
            return true;
        }
        self.active.waker.register(cx.waker());
        self.active.revoked.load(Ordering::Acquire)
    }
}

impl Drop for TrustedPeerConnection {
    fn drop(&mut self) {
        self.registry.remove(self.peer, self.id);
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    #[tokio::test]
    async fn trusted_peer_replacement_revokes_active_and_late_tracked_connections() {
        let registry = TrustedPeerConnections::default();
        let peer = HostId::from_u128(2);
        let (active_io, _active_peer) = tokio::io::duplex(64);
        let mut active = BoxedGrpcIo::trusted(active_io, peer).track_trusted_peer(&registry);

        assert_eq!(registry.close_host(peer).await, 1);
        let error = active.write_all(b"x").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);

        let (late_io, _late_peer) = tokio::io::duplex(64);
        let mut late = BoxedGrpcIo::trusted(late_io, peer).track_trusted_peer(&registry);
        let error = late.write_all(b"x").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);

        registry.finish_host_replacement(peer);
        let (fresh_io, _fresh_peer) = tokio::io::duplex(64);
        let mut fresh = BoxedGrpcIo::trusted(fresh_io, peer).track_trusted_peer(&registry);
        fresh.write_all(b"x").await.unwrap();
    }
}
