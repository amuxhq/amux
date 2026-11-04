//! Common tonic IO wrapper for transports that do not need connect metadata.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

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
    /// Through a paired host forwarding for both.
    Forwarded,
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
        }
    }
}

impl AsyncRead for BoxedGrpcIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedGrpcIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
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
